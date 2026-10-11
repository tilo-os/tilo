//! Typed, validated compositor-facing hardware preferences.
//!
//! Settings is not allowed to invent capabilities: callers pass a live
//! [`OutputDescriptor`] to [`DisplayConfig::validate_for`] and only the
//! resulting plan may be applied.  The file is durable intent; DRM/libinput
//! remain the authority for what the currently attached hardware can do.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tilo_clock::unix_now_ms;

mod profiles;

pub use profiles::{
    AttachedOutput, DisplayProfile, DisplayProfiles, DisplaySet, MonitorHistory, same_scale,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputTransform {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
}

/// An output's extent in the logical desktop layout: its `mode` divided by
/// its UI `scale`, then rotated. [`OutputConfig::x`]/[`OutputConfig::y`] are
/// in these units, so outputs placed edge to edge use this extent — not the
/// mode — or a 2x panel leaves a gap as wide as itself beside it.
///
/// Rounded the way `zxdg_output_v1.logical_size` is (nearest pixel per
/// axis), so the stored layout and what clients are told agree exactly. A
/// non-positive or non-finite scale counts as 1.
pub fn logical_extent(mode: (u32, u32), scale: f64, transform: OutputTransform) -> (u32, u32) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded and clamped to [0, u32::MAX] first"
    )]
    let axis = |px: u32| {
        (f64::from(px) / scale)
            .round()
            .clamp(0.0, f64::from(u32::MAX)) as u32
    };
    let (w, h) = (axis(mode.0), axis(mode.1));
    if matches!(
        transform,
        OutputTransform::Rotate90 | OutputTransform::Rotate270
    ) {
        (h, w)
    } else {
        (w, h)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputMode {
    pub width: u32,
    pub height: u32,
    /// Milli-Hz keeps 59.94 and 60 Hz distinct.
    pub refresh_mhz: u32,
}

impl OutputMode {
    /// How a person reads the mode: `1920 × 1080 at 60 Hz`, with the rate's
    /// fraction only when it has one (`59.94 Hz`).
    pub fn label(self) -> String {
        let hz = format!("{:.2}", f64::from(self.refresh_mhz) / 1000.0);
        let hz = hz.trim_end_matches('0').trim_end_matches('.');
        format!("{} × {} at {hz} Hz", self.width, self.height)
    }
}

/// What a sink's EDID says it is. Empty strings mean the EDID did not say.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorIdentity {
    /// Manufacturer name, or its three-letter PNP id when the name is not
    /// known, e.g. `Dell` or `XYZ`.
    pub make: String,
    /// The monitor-name descriptor, else the product code, e.g.
    /// `DELL U2720Q` or `0x40a4`.
    pub model: String,
    /// The serial-string descriptor, else the numeric serial when non-zero.
    pub serial: String,
    pub physical_size_mm: Option<(u32, u32)>,
}

impl MonitorIdentity {
    /// The name a person recognises, e.g. `Dell U2720Q`. Monitor-name
    /// descriptors usually already start with the brand, so it is not
    /// repeated.
    pub fn display_name(&self) -> String {
        let make = self.make.trim();
        let model = self.model.trim();
        let model_names_make = model
            .split_whitespace()
            .next()
            .is_some_and(|first| first.eq_ignore_ascii_case(make));
        match (make.is_empty(), model.is_empty()) {
            (_, true) => make.to_owned(),
            (true, false) => model.to_owned(),
            (false, false) if model_names_make => {
                let rest = model.get(make.len()..).unwrap_or_default();
                format!("{make}{rest}")
            }
            (false, false) => format!("{make} {model}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputDescriptor {
    /// Collision-free output key used by persisted display intent. This is
    /// the familiar DRM connector label when it is unique, and a
    /// card-qualified key only when another card exposes the same label.
    pub connector: String,
    /// Raw connector label exposed by DRM, e.g. `eDP-1` or `HDMI-A-1`.
    /// Older compositor snapshots omit this field; consumers must then use
    /// [`Self::connector`] as the user-facing fallback.
    #[serde(default)]
    pub connector_label: String,
    pub modes: Vec<OutputMode>,
    pub hdr: bool,
    pub vrr: bool,
    pub color_profiles: Vec<String>,
    /// EDID identity, `None` for a sink that exposes no readable EDID.
    pub monitor: Option<MonitorIdentity>,
}

impl OutputDescriptor {
    /// Connector label suitable for a compact user-facing description. The
    /// persisted key stays available in [`Self::connector`] for selections,
    /// mirrors and widget ids.
    pub fn label(&self) -> &str {
        if self.connector_label.is_empty() {
            &self.connector
        } else {
            &self.connector_label
        }
    }

    /// The card portion of a collision-resolved key, if this descriptor had
    /// to be qualified. Keys use `card-identity/raw-connector`; a raw
    /// connector label never contains `/` in DRM's connector-name grammar.
    pub fn card_qualifier(&self) -> Option<&str> {
        self.connector
            .strip_suffix(self.label())
            .and_then(|prefix| prefix.strip_suffix('/'))
    }
}

/// One compositor-published output capability snapshot.  Settings consumes
/// this instead of probing DRM itself: the process that owns KMS is the only
/// authority that can say which modes and output features are actually
/// usable by the active renderer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayOutputSnapshot {
    pub descriptor: OutputDescriptor,
    pub current_mode: OutputMode,
    /// The scale the compositor shows this output at: its live scale while
    /// it is on, else the one it would choose for it unconfigured. An output
    /// with no saved entry runs at the compositor's own choice, so this is
    /// the scale a layout must keep when it first writes one.
    pub scale: f64,
    pub builtin: bool,
    /// The sink/EDID may support HDR even when the active scanout pipeline
    /// cannot safely signal or encode it. `descriptor.hdr` is the usable
    /// capability; this field explains the distinction without creating a
    /// decorative toggle.
    pub sink_hdr: bool,
    /// Why a sink that accepts HDR cannot be driven in HDR here. Typed so
    /// Settings can explain it in the user's language.
    pub hdr_unavailable_reason: Option<HdrUnavailable>,
    pub sink_vrr: bool,
    /// Why a sink that reports adaptive sync cannot use it here. Typed so
    /// Settings can explain it in the user's language.
    pub vrr_unavailable_reason: Option<AdaptiveSyncUnavailable>,
    /// Stable profile identifier (currently the canonical ICC path) to a
    /// user-facing label. Only profiles in this map may be selected.
    pub color_profile_labels: BTreeMap<String, String>,
}

impl DisplayOutputSnapshot {
    /// The entry that describes this output as it runs with none saved: the
    /// defaults at the compositor's own scale.
    pub fn unconfigured(&self) -> OutputConfig {
        OutputConfig::at_scale(self.scale)
    }
}

/// Why an HDR-capable sink cannot be driven in HDR by the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrUnavailable {
    /// The kernel driver lacks a connector property HDR signalling needs:
    /// `HDR_OUTPUT_METADATA`, a `BT2020_RGB` `Colorspace`, or a `max bpc`
    /// of at least 10.
    DriverUnsupported,
    /// No 10-bit or half-float scanout buffer could be created or shown.
    FormatUnsupported,
    /// The driver refused the HDR signal or the mode change it needs.
    DriverRejected,
}

/// Why an adaptive-sync-capable sink cannot have it enabled by the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveSyncUnavailable {
    /// The kernel driver offers no `VRR_ENABLED` property on any CRTC that
    /// could drive this connector.
    DriverUnsupported,
    /// The driver refused to enable adaptive sync when it was requested.
    DriverRejected,
}

/// Read-only state written atomically by the compositor in the private user
/// runtime directory. It is deliberately separate from durable intent:
/// unplugging a monitor changes this document, not the user's saved layout.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayHardwareSnapshot {
    pub generated_unix_ms: u64,
    pub backend: String,
    pub publisher_pid: u32,
    /// Linux `/proc/<pid>/stat` start-time ticks. With the PID it names the
    /// publisher's lease (see [`publisher_identity`]), so a stale runtime file
    /// cannot become trusted after PID reuse.
    pub publisher_start_ticks: u64,
    pub outputs: Vec<DisplayOutputSnapshot>,
    /// Where each enabled output sits in the compositor's combined desktop
    /// coordinates right now, after arrangement, mirroring and rotation.
    pub layout: Vec<LiveOutputPlacement>,
}

/// One enabled output's live rectangle in combined desktop coordinates.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveOutputPlacement {
    /// Collision-free output key, as in [`OutputDescriptor::connector`].
    pub connector: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl DisplayHardwareSnapshot {
    pub fn load() -> Option<Self> {
        let path = display_snapshot_path()?;
        let snapshot: Self = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
        publisher_live_in(
            path.parent()?,
            snapshot.publisher_pid,
            snapshot.publisher_start_ticks,
        )
        .then_some(snapshot)
    }

    /// # Errors
    ///
    /// Returns an error if `XDG_RUNTIME_DIR` is unset or relative, `self`
    /// can't be serialised, or the snapshot file can't be written.
    pub fn save(&self) -> std::io::Result<()> {
        let path = display_snapshot_path()
            .ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR is unavailable"))?;
        let mut body = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        body.push(b'\n');
        tilo_files::native::atomic::write_creating_parent(&path, &body)
    }
}

/// Mark this process as the live publisher of runtime snapshots in the
/// private runtime directory for the rest of its lifetime, and return the
/// identity to stamp on what it publishes.
///
/// # Errors
///
/// As [`publisher_identity_in`], or when `XDG_RUNTIME_DIR` is unavailable.
pub fn publisher_identity() -> std::io::Result<(u32, u64)> {
    let path = display_snapshot_path()
        .ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR is unavailable"))?;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("display snapshot path has no parent"))?;
    publisher_identity_in(dir)
}

/// Readers cannot check a publisher through `/proc/<pid>`: the compositor
/// makes itself non-dumpable while it holds identity keys, so its `/proc`
/// entries belong to root, and a `/proc` mounted with `hidepid` hides it even
/// from its own user. So the publisher holds an exclusive
/// `flock` on `publisher-<pid>-<start ticks>.lock` beside its snapshots for
/// as long as it lives; the kernel releases it however the process ends, and
/// a PID with a different start time names a different file.
///
/// # Errors
///
/// When this process's start time is unreadable, or the lease cannot be
/// created or locked.
pub fn publisher_identity_in(dir: &std::path::Path) -> std::io::Result<(u32, u64)> {
    static LEASES: std::sync::Mutex<Vec<(std::path::PathBuf, advisory_lock::FileLock)>> =
        std::sync::Mutex::new(Vec::new());
    let pid = std::process::id();
    let start_ticks = process_start_ticks(pid)
        .ok_or_else(|| std::io::Error::other("this process has no readable start time"))?;
    let path = publisher_lease_path(dir, pid, start_ticks);
    let mut leases = LEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !leases.iter().any(|(held, _)| *held == path) {
        tilo_files::native::create_dir_all(dir)?;
        let lease = advisory_lock::FileLock::try_open(&path, advisory_lock::LockMode::Exclusive)
            .map_err(|error| match error {
                advisory_lock::TryLockError::WouldBlock => {
                    std::io::Error::other("another open file already holds this publisher's lease")
                }
                advisory_lock::TryLockError::Error(error) => error,
            })?;
        leases.push((path, lease));
    }
    drop(leases);
    Ok((pid, start_ticks))
}

/// Whether the publisher `pid`/`start_ticks` still holds its lease in `dir`.
pub fn publisher_live_in(dir: &std::path::Path, pid: u32, start_ticks: u64) -> bool {
    if pid == 0 {
        return false;
    }
    let Ok(lease) = std::fs::File::open(publisher_lease_path(dir, pid, start_ticks)) else {
        return false;
    };
    matches!(
        advisory_lock::FileLock::try_shared(lease),
        Err(advisory_lock::TryLockError::WouldBlock)
    )
}

fn publisher_lease_path(dir: &std::path::Path, pid: u32, start_ticks: u64) -> std::path::PathBuf {
    dir.join(format!("publisher-{pid}-{start_ticks}.lock"))
}

pub fn process_start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is parenthesised and may contain spaces or `)`, so
    // split only after its final closing paren. starttime is field 22, which
    // is index 19 in the remainder beginning at field 3.
    let (_, remainder) = stat.rsplit_once(") ")?;
    remainder.split_whitespace().nth(19)?.parse().ok()
}

/// The compositor/Settings handshake for a potentially screen-blanking
/// change. Settings writes `Requested`; the compositor validates and applies
/// it, then changes it to `AwaitingConfirmation`. Only `ConfirmRequested`
/// commits durable intent. A timeout or `RevertRequested` restores
/// `previous`, including after Settings itself crashed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayTransactionState {
    #[default]
    Requested,
    AwaitingConfirmation,
    ConfirmRequested,
    RevertRequested,
    Confirmed,
    Reverted,
    Rejected,
}

/// The section a [`DisplayTransaction`] is stored in, whole.
pub const DISPLAY_TRANSACTION_SECTION: &str = "display_transaction";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayTransaction {
    pub id: u64,
    pub state: DisplayTransactionState,
    /// The monitors the candidate was arranged for. It applies only while
    /// exactly this set is attached, and is remembered as its layout.
    pub set: DisplaySet,
    pub previous: DisplayConfig,
    pub candidate: DisplayConfig,
    pub deadline_unix_ms: Option<u64>,
    pub message: Option<String>,
}

impl Default for DisplayTransaction {
    fn default() -> Self {
        Self {
            id: 0,
            state: DisplayTransactionState::Requested,
            set: DisplaySet::default(),
            previous: DisplayConfig::default(),
            candidate: DisplayConfig::default(),
            deadline_unix_ms: None,
            message: None,
        }
    }
}

impl DisplayTransaction {
    pub const CONFIRM_TIMEOUT_MS: u64 = 15_000;

    /// # Errors
    ///
    /// Returns an error if the transaction can't be serialised or written.
    pub fn request(set: DisplaySet, candidate: DisplayConfig) -> std::io::Result<Self> {
        let transaction = Self::prepare_request(set, candidate)?;
        transaction.save()?;
        Ok(transaction)
    }

    /// Construct an admitted request without writing storage. Its identity
    /// stays unique among requests in this process even within one clock tick.
    /// The request becomes effective only after its normal durable save.
    ///
    /// # Errors
    /// Returns an error if the request identity counter is exhausted.
    pub fn prepare_request(set: DisplaySet, candidate: DisplayConfig) -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static LAST_REQUEST: AtomicU64 = AtomicU64::new(0);
        let now = unix_now_ms();
        let previous = LAST_REQUEST
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |previous| {
                previous.checked_add(1).map(|next| next.max(now))
            })
            .map_err(|_| std::io::Error::other("display request identities exhausted"))?;
        Ok(Self {
            id: previous.saturating_add(1).max(now),
            state: DisplayTransactionState::Requested,
            previous: DisplayConfig::load_for(&set),
            set,
            candidate,
            deadline_unix_ms: None,
            message: None,
        })
    }

    pub fn load() -> Option<Self> {
        Self::from_section(super::load_section("display_transaction"))
    }

    fn from_section(section: serde_json::Map<String, serde_json::Value>) -> Option<Self> {
        // `load_section` intentionally represents a missing section as an
        // empty map. Do not deserialize that through this type's serde
        // defaults: it would manufacture a phantom `Requested` transaction
        // whose empty candidate overrides the committed layout.
        if !section.contains_key("id") {
            return None;
        }
        let value = serde_json::Value::Object(section);
        serde_json::from_value(value).ok()
    }

    /// # Errors
    ///
    /// Returns an error if `self` can't be serialised or the section can't
    /// be written.
    pub fn save(&self) -> std::io::Result<()> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        super::update_section(
            "display_transaction",
            value.as_object().cloned().unwrap_or_default(),
        )
    }

    /// # Errors
    ///
    /// Returns an error if the transitioned state can't be [`saved`](Self::save).
    pub fn request_confirmation(&mut self) -> std::io::Result<()> {
        if self.state == DisplayTransactionState::AwaitingConfirmation {
            self.state = DisplayTransactionState::ConfirmRequested;
            self.save()?;
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if the transitioned state can't be [`saved`](Self::save).
    pub fn request_revert(&mut self) -> std::io::Result<()> {
        if matches!(
            self.state,
            DisplayTransactionState::Requested
                | DisplayTransactionState::AwaitingConfirmation
                | DisplayTransactionState::ConfirmRequested
        ) {
            self.state = DisplayTransactionState::RevertRequested;
            self.save()?;
        }
        Ok(())
    }

    pub fn expired(&self, now_unix_ms: u64) -> bool {
        self.state == DisplayTransactionState::AwaitingConfirmation
            && self.deadline_unix_ms.is_some_and(|d| now_unix_ms >= d)
    }
}

/// Private session state path. A relative or absent `XDG_RUNTIME_DIR` is not
/// accepted: falling back to `/tmp` would let another user forge display
/// capabilities shown by Settings.
pub fn display_snapshot_path() -> Option<std::path::PathBuf> {
    tilo_paths::xdg::runtime_dir().map(|path| path.join("tilo/display-state.json"))
}

/// What kind of input device libinput reports, as far as Settings cares:
/// each kind has its own section, shown only while one is attached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputDeviceKind {
    Touchpad,
    /// Any relative pointer that is not a touchpad: mice, trackballs,
    /// pointing sticks.
    Mouse,
    Keyboard,
    Touchscreen,
    /// A drawing tablet's pen digitiser.
    Tablet,
    /// The button pad on a drawing tablet.
    TabletPad,
    #[default]
    #[serde(other)]
    Other,
}

/// The identity input preferences are keyed by: USB vendor and product id
/// plus the kernel's device name, so two identical tablets share settings
/// while a tablet's pen and pad (same ids, different names) do not.
pub fn input_device_key(vendor: u32, product: u32, name: &str) -> String {
    format!("{vendor:04x}:{product:04x}:{}", name.trim())
}

/// One attached input device, as the compositor sees it right now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputDeviceSnapshot {
    /// [`input_device_key`] of the device.
    pub key: String,
    pub name: String,
    pub kind: InputDeviceKind,
    /// Physical buttons on a tablet pad; zero for every other kind.
    pub pad_buttons: u32,
    /// A tablet pad's touch rings, in libinput's order: how many modes each
    /// one's mode group has.
    pub pad_rings: Vec<u32>,
    /// A tablet pad's touch strips, like [`Self::pad_rings`].
    pub pad_strips: Vec<u32>,
    /// Pad buttons (libinput's numbers, from zero) that switch their mode
    /// group to its next mode rather than doing anything themselves.
    pub pad_mode_buttons: Vec<u32>,
}

/// Read-only state written atomically by the compositor in the private user
/// runtime directory: the input devices attached right now. Like
/// [`DisplayHardwareSnapshot`] it is separate from durable intent, so
/// unplugging a device hides its section without forgetting its settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputHardwareSnapshot {
    pub publisher_pid: u32,
    /// With the PID it names the publisher's lease, so a stale file cannot
    /// outlive its compositor.
    pub publisher_start_ticks: u64,
    pub devices: Vec<InputDeviceSnapshot>,
    /// Last typing keyboard; presentation state, never a saved assignment.
    pub active_keyboard: Option<String>,
}

impl InputHardwareSnapshot {
    /// The live snapshot, or `None` when no running compositor published one.
    pub fn load() -> Option<Self> {
        Self::load_at(&input_snapshot_path()?)
    }

    pub fn load_at(path: &std::path::Path) -> Option<Self> {
        let snapshot: Self = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        publisher_live_in(
            path.parent()?,
            snapshot.publisher_pid,
            snapshot.publisher_start_ticks,
        )
        .then_some(snapshot)
    }

    /// # Errors
    ///
    /// Returns an error if `XDG_RUNTIME_DIR` is unset or relative, or the
    /// snapshot file can't be written.
    pub fn save(&self) -> std::io::Result<()> {
        let path = input_snapshot_path()
            .ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR is unavailable"))?;
        self.save_at(&path)
    }

    /// # Errors
    ///
    /// Returns an error if `self` can't be serialised or written to `path`.
    pub fn save_at(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut body = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        body.push(b'\n');
        tilo_files::native::atomic::write_creating_parent(path, &body)
    }

    /// Whether at least one device of `kind` is attached.
    pub fn has(&self, kind: InputDeviceKind) -> bool {
        self.devices.iter().any(|device| device.kind == kind)
    }

    /// The attached devices of `kind`, one entry per identity.
    pub fn of_kind(&self, kind: InputDeviceKind) -> Vec<&InputDeviceSnapshot> {
        let mut seen = BTreeSet::new();
        self.devices
            .iter()
            .filter(|device| device.kind == kind && seen.insert(device.key.as_str()))
            .collect()
    }
}

/// Private session state path for [`InputHardwareSnapshot`]; like
/// [`display_snapshot_path`] it never falls back to a shared directory.
pub fn input_snapshot_path() -> Option<std::path::PathBuf> {
    tilo_paths::xdg::runtime_dir().map(|path| path.join("tilo/input-state.json"))
}

/// `input` key holding per-tablet settings, keyed by [`input_device_key`].
pub const TABLETS_KEY: &str = "tablets";

/// What a pen barrel button does. `Default` reports the button as itself:
/// tablet-aware apps see a pen button, everything else a right (lower
/// button) or middle (upper button) click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StylusButtonAction {
    #[default]
    Default,
    Left,
    Middle,
    Right,
    Back,
    Forward,
    /// Press the chord bound to this desktop shortcut.
    Shortcut(ShortcutAction),
    Disabled,
}

/// What the eraser end of a pen does. `Native` reports it as an eraser:
/// tablet-aware apps erase with it, everything else sees a left click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EraserAction {
    #[default]
    Native,
    /// Touching the surface presses this mouse button in every app.
    Left,
    Middle,
    Right,
    Back,
    Forward,
    /// Touching the surface presses the chord bound to this shortcut.
    Shortcut(ShortcutAction),
}

/// What turning a pad's touch ring or sliding along its strip does in an app
/// that does not take tablet input itself. Tablet-aware apps always get the
/// ring and strip as they are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadDialAction {
    Nothing,
    /// Scroll whatever is under the pointer.
    #[default]
    Scroll,
    /// Zoom in and out (Ctrl+= and Ctrl+-).
    Zoom,
    /// Grow and shrink the brush (`]` and `[`), as drawing apps bind them.
    BrushSize,
    /// Step the output volume.
    Volume,
    /// Step the display brightness.
    Brightness,
}

/// A pad control that turns or slides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PadDial {
    Ring(u32),
    Strip(u32),
}

/// Pressure curves Settings offers, softest first, as Bézier control points
/// `[x1, y1, x2, y2]` between `(0, 0)` and `(1, 1)`. The middle one is linear.
pub const PRESSURE_CURVE_PRESETS: [[f64; 4]; 7] = [
    [0.0, 0.75, 0.25, 1.0],
    [0.0, 0.5, 0.5, 1.0],
    [0.0, 0.25, 0.75, 1.0],
    [0.0, 0.0, 1.0, 1.0],
    [0.25, 0.0, 1.0, 0.75],
    [0.5, 0.0, 1.0, 0.5],
    [0.75, 0.0, 1.0, 0.25],
];

/// The preset for a soft (`0.0`) … firm (`1.0`) slider position.
pub fn pressure_curve_for_feel(feel: f64) -> [f64; 4] {
    let feel = if feel.is_finite() {
        feel.clamp(0.0, 1.0)
    } else {
        0.5
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "feel is clamped to [0, 1], so the index is within 0..=6"
    )]
    let index = (feel * 6.0).round() as usize;
    PRESSURE_CURVE_PRESETS
        .get(index)
        .copied()
        .unwrap_or(PRESSURE_CURVE_PRESETS[3])
}

/// The slider position of the preset nearest to `curve`.
pub fn feel_for_pressure_curve(curve: [f64; 4]) -> f64 {
    let distance = |preset: &[f64; 4]| -> f64 {
        preset
            .iter()
            .zip(curve.iter())
            .map(|(a, b)| (a - b).powi(2))
            .sum()
    };
    let nearest = PRESSURE_CURVE_PRESETS
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))
        .map_or(3, |(index, _)| index);
    #[allow(clippy::cast_precision_loss, reason = "index is at most 6")]
    let position = nearest as f64 / 6.0;
    position
}

/// One drawing tablet's settings, stored per device identity so each
/// tablet keeps its own mapping when several are used.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabletConfig {
    /// Collision-free output key ([`OutputDescriptor::connector`]) the
    /// tablet maps onto; `None` spans the whole desktop.
    pub output: Option<String>,
    /// Use only the part of the tablet that has the target's shape, so
    /// strokes are not stretched.
    pub keep_aspect: bool,
    /// Turn the tablet half a turn, for use with its buttons on the right.
    pub left_handed: bool,
    /// Bézier control points `[x1, y1, x2, y2]`; see
    /// [`PRESSURE_CURVE_PRESETS`].
    pub pressure_curve: [f64; 4],
    /// The barrel button nearest the tip.
    pub lower_button: StylusButtonAction,
    /// The barrel button further from the tip.
    pub upper_button: StylusButtonAction,
    /// A third barrel button, on pens that have one.
    pub third_button: StylusButtonAction,
    /// What the eraser end of the pen does.
    pub eraser: EraserAction,
    /// Pad button number (libinput's, from zero) to the shortcut it runs.
    pub pad_buttons: BTreeMap<u32, ShortcutAction>,
    /// The pad button that shows what every control does while held, as
    /// the mode buttons do. It runs no shortcut of its own.
    pub pad_map_button: Option<u32>,
    /// Pad ring number to what it does in each mode of its mode group,
    /// indexed by mode. Modes past the end scroll.
    pub pad_rings: BTreeMap<u32, Vec<PadDialAction>>,
    /// Pad strip number to what it does in each mode, like
    /// [`Self::pad_rings`].
    pub pad_strips: BTreeMap<u32, Vec<PadDialAction>>,
}

impl TabletConfig {
    fn dials(&self, dial: PadDial) -> (&BTreeMap<u32, Vec<PadDialAction>>, u32) {
        match dial {
            PadDial::Ring(index) => (&self.pad_rings, index),
            PadDial::Strip(index) => (&self.pad_strips, index),
        }
    }

    /// What `dial` does in `mode` for an app without tablet support.
    pub fn dial_action(&self, dial: PadDial, mode: u32) -> PadDialAction {
        let (dials, index) = self.dials(dial);
        dials
            .get(&index)
            .and_then(|modes| modes.get(usize::try_from(mode).ok()?))
            .copied()
            .unwrap_or_default()
    }

    /// Make `dial` do `action` in `mode`, leaving its other modes as they are.
    pub fn set_dial_action(&mut self, dial: PadDial, mode: u32, action: PadDialAction) {
        let (dials, index) = match dial {
            PadDial::Ring(index) => (&mut self.pad_rings, index),
            PadDial::Strip(index) => (&mut self.pad_strips, index),
        };
        let Ok(mode) = usize::try_from(mode) else {
            return;
        };
        let modes = dials.entry(index).or_default();
        if modes.len() <= mode {
            modes.resize(mode.saturating_add(1), PadDialAction::default());
        }
        if let Some(slot) = modes.get_mut(mode) {
            *slot = action;
        }
        while modes.last() == Some(&PadDialAction::default()) {
            modes.pop();
        }
        if modes.is_empty() {
            dials.remove(&index);
        }
    }
}

impl Default for TabletConfig {
    fn default() -> Self {
        Self {
            output: None,
            keep_aspect: false,
            left_handed: false,
            pressure_curve: PRESSURE_CURVE_PRESETS[3],
            lower_button: StylusButtonAction::Default,
            upper_button: StylusButtonAction::Default,
            third_button: StylusButtonAction::Default,
            eraser: EraserAction::Native,
            pad_buttons: BTreeMap::new(),
            pad_map_button: None,
            pad_rings: BTreeMap::new(),
            pad_strips: BTreeMap::new(),
        }
    }
}

/// Every tablet's settings from an `input` section. One malformed tablet
/// entry is skipped rather than discarding the others.
pub fn tablets_from_section(
    section: &serde_json::Map<String, serde_json::Value>,
) -> BTreeMap<String, TabletConfig> {
    section
        .get(TABLETS_KEY)
        .map(tablets_from_value)
        .unwrap_or_default()
}

fn tablets_from_value(value: &serde_json::Value) -> BTreeMap<String, TabletConfig> {
    value
        .as_object()
        .map(|tablets| {
            tablets
                .iter()
                .filter_map(|(key, value)| {
                    serde_json::from_value(value.clone())
                        .ok()
                        .map(|config| (key.clone(), config))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Serde half of [`tablets_from_section`], for `deserialize_with`: one bad
/// tablet cannot make the whole `input` section unreadable.
///
/// # Errors
///
/// Only when the input is not JSON at all; malformed tablets are skipped.
pub fn lenient_tablets<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, TabletConfig>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(tablets_from_value(&value))
}

// Four independent, user-facing toggles (Enabled, Primary, HDR, VRR), not a state
// machine — and this document's JSON shape and field names are load-bearing across
// `apps/settings` and `shell`, so folding them into a bitflags type ripples well
// outside this crate.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent per-output toggles serialised by field name; consumers in \
              other crates read and persist them directly"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub enabled: bool,
    pub primary: bool,
    /// Logical top-left in the shared infinite workspace plane.
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: OutputTransform,
    pub mode: Option<OutputMode>,
    pub mirror_of: Option<String>,
    pub hdr: bool,
    /// How bright ordinary (SDR) content is while this output runs in HDR,
    /// in cd/m². 203 is the ITU BT.2408 reference white.
    pub sdr_brightness_nits: u16,
    /// Adaptive sync is allowed on this output. The compositor engages it
    /// only while a fullscreen window is shown there (games, video) and
    /// keeps the fixed refresh rate for the desktop, where uneven frame
    /// times make many panels flicker. Inert on a sink that cannot do it.
    pub vrr: bool,
    pub color_profile: Option<String>,
}

impl OutputConfig {
    /// The defaults at `scale`, or at 1x when `scale` is not a usable
    /// factor.
    pub fn at_scale(scale: f64) -> Self {
        Self {
            scale: if scale.is_finite() && scale > 0.0 {
                scale
            } else {
                1.0
            },
            ..Self::default()
        }
    }
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            primary: false,
            x: 0,
            y: 0,
            scale: 1.0,
            transform: OutputTransform::Normal,
            mode: None,
            mirror_of: None,
            hdr: false,
            sdr_brightness_nits: DEFAULT_SDR_BRIGHTNESS_NITS,
            vrr: true,
            color_profile: None,
        }
    }
}

/// SDR content brightness on an HDR output unless the user picks another:
/// the ITU BT.2408 reference white, where HDR video expects SDR white.
pub const DEFAULT_SDR_BRIGHTNESS_NITS: u16 = 203;
/// The SDR content brightness range Settings offers, cd/m².
pub const SDR_BRIGHTNESS_NITS_RANGE: std::ops::RangeInclusive<u16> = 80..=480;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    pub outputs: BTreeMap<String, OutputConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayConfigError {
    NoEnabledOutput,
    MultiplePrimary,
    UnknownMirrorTarget { output: String, target: String },
    MirrorCycle { output: String },
    IncompatibleMirrorModes { output: String, target: String },
    UnsupportedMode { output: String },
    InvalidScale { output: String },
    UnsupportedHdr { output: String },
    UnsupportedColorProfile { output: String },
}

impl DisplayConfig {
    /// Look up intent by the compositor-published collision-free output key.
    /// A legacy raw connector entry is deliberately not a fallback for a
    /// card-qualified key: applying it to both same-named GPU connectors
    /// would turn one user's layout into two conflicting layouts.
    pub fn output_for(&self, connector: &str) -> Option<&OutputConfig> {
        self.outputs.get(connector)
    }

    /// Validate intent against a fresh hardware snapshot. Unknown/disconnected
    /// configured outputs are retained for re-plug but ignored in this plan.
    ///
    /// # Errors
    ///
    /// Returns the specific [`DisplayConfigError`] variant naming the first
    /// rule an enabled, attached output's configuration violates — no
    /// enabled output, more than one primary, an unsupported mode, scale,
    /// HDR or color profile, or an unresolved or cyclic mirror target.
    /// Allowing adaptive sync is never an error: it only engages where the
    /// sink and driver can do it.
    pub fn validate_for(&self, attached: &[OutputDescriptor]) -> Result<(), DisplayConfigError> {
        let names: BTreeSet<&str> = attached.iter().map(|o| o.connector.as_str()).collect();
        let live: Vec<(&OutputDescriptor, &OutputConfig)> = attached
            .iter()
            .filter_map(|d| self.outputs.get(&d.connector).map(|c| (d, c)))
            .collect();
        if !attached.is_empty() && !live.iter().any(|(_, c)| c.enabled) {
            return Err(DisplayConfigError::NoEnabledOutput);
        }
        if live.iter().filter(|(_, c)| c.enabled && c.primary).count() > 1 {
            return Err(DisplayConfigError::MultiplePrimary);
        }
        for (desc, cfg) in live {
            if !cfg.scale.is_finite() || !(0.5..=4.0).contains(&cfg.scale) {
                return Err(DisplayConfigError::InvalidScale {
                    output: desc.connector.clone(),
                });
            }
            if let Some(mode) = cfg.mode
                && !desc.modes.contains(&mode)
            {
                return Err(DisplayConfigError::UnsupportedMode {
                    output: desc.connector.clone(),
                });
            }
            if cfg.hdr && !desc.hdr {
                return Err(DisplayConfigError::UnsupportedHdr {
                    output: desc.connector.clone(),
                });
            }
            if let Some(profile) = &cfg.color_profile
                && !desc.color_profiles.contains(profile)
            {
                return Err(DisplayConfigError::UnsupportedColorProfile {
                    output: desc.connector.clone(),
                });
            }
            if let Some(target) = &cfg.mirror_of {
                if !names.contains(target.as_str()) {
                    return Err(DisplayConfigError::UnknownMirrorTarget {
                        output: desc.connector.clone(),
                        target: target.clone(),
                    });
                }
                let mut seen = BTreeSet::from([desc.connector.as_str()]);
                let mut cursor = target.as_str();
                while let Some(next) = self
                    .outputs
                    .get(cursor)
                    .and_then(|c| c.mirror_of.as_deref())
                {
                    if !seen.insert(cursor) || seen.contains(next) {
                        return Err(DisplayConfigError::MirrorCycle {
                            output: desc.connector.clone(),
                        });
                    }
                    cursor = next;
                }
                let Some(target_desc) = attached
                    .iter()
                    .find(|candidate| candidate.connector == *target)
                else {
                    continue;
                };
                let target_mode = self.outputs.get(target).and_then(|output| output.mode);
                // Tilo renders the shared viewport into each connector's
                // scanout independently. Mirroring therefore needs a common
                // pixel extent, not an identical timing: a 60.000 Hz laptop
                // panel and a 59.940 Hz USB-C sink can show the same viewport
                // at their own presentation cadences.
                let same_extent = |left: OutputMode, right: OutputMode| {
                    left.width == right.width && left.height == right.height
                };
                let compatible = match (cfg.mode, target_mode) {
                    (Some(source), Some(target)) => same_extent(source, target),
                    _ => desc.modes.iter().any(|source| {
                        target_desc
                            .modes
                            .iter()
                            .any(|target| same_extent(*source, *target))
                    }),
                };
                if !compatible {
                    return Err(DisplayConfigError::IncompatibleMirrorModes {
                        output: desc.connector.clone(),
                        target: target.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// The committed layout for this set of monitors, empty for a set never
    /// arranged before.
    pub fn load_for(set: &DisplaySet) -> Self {
        DisplayProfiles::load().layout_for(set).unwrap_or_default()
    }

    /// Configuration the compositor should apply right now. A staged
    /// transaction for this same set temporarily overrides committed intent
    /// while the user can still see and confirm the result;
    /// rejection/revert always resolves to the last known-good
    /// configuration. A transaction staged for other monitors never applies.
    pub fn load_effective(set: &DisplaySet) -> Self {
        let committed = Self::load_for(set);
        let Some(transaction) = DisplayTransaction::load() else {
            return committed;
        };
        if transaction.set.key != set.key {
            return committed;
        }
        match transaction.state {
            DisplayTransactionState::Requested
            | DisplayTransactionState::AwaitingConfirmation
            | DisplayTransactionState::ConfirmRequested => transaction.candidate,
            DisplayTransactionState::RevertRequested => transaction.previous,
            DisplayTransactionState::Confirmed
            | DisplayTransactionState::Reverted
            | DisplayTransactionState::Rejected => committed,
        }
    }

    /// Remember this as the layout for `set`.
    ///
    /// # Errors
    ///
    /// Returns an error if the profiles can't be serialised or written.
    pub fn save_for(&self, set: &DisplaySet) -> std::io::Result<()> {
        let mut profiles = DisplayProfiles::load();
        profiles.remember(set, self);
        profiles.save()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccelProfile {
    Flat,
    #[default]
    Adaptive,
}

// Seven independent, user-facing input toggles, not a state machine — and this
// document's JSON shape and field names are load-bearing across `apps/settings`
// and `shell`, so folding them into a bitflags type ripples well outside this
// crate.
/// Fedora GNOME-compatible defaults: touchpads move content with the fingers,
/// while physical mouse wheels retain their conventional direction.
pub const DEFAULT_TOUCHPAD_NATURAL_SCROLL: bool = true;
pub const DEFAULT_MOUSE_NATURAL_SCROLL: bool = false;
/// `input` key for whether touchpads send events at all. Absent means on;
/// the touchpad-toggle hardware key flips it and the compositor applies it
/// to every touchpad through libinput's send-events mode.
pub const TOUCHPAD_ENABLED_KEY: &str = "touchpad_enabled";
/// `input` key for switching touchpads off while an external mouse is
/// connected (libinput's disabled-on-external-mouse send-events mode). Absent
/// means off.
pub const TOUCHPAD_DISABLED_ON_EXTERNAL_MOUSE_KEY: &str = "touchpad_disabled_on_external_mouse";

/// The section of [`RichInputConfig`]: pointer, touchpad and keyboard.
pub const INPUT_SECTION: &str = "input";
/// The section of [`DisplayConfig`] and the display toggles below.
pub const DISPLAY_SECTION: &str = "display";

/// Whether touchpads send events at all ([`TOUCHPAD_ENABLED_KEY`]).
pub const TOUCHPAD_ENABLED: crate::Key<bool> =
    crate::Key::new(INPUT_SECTION, TOUCHPAD_ENABLED_KEY, || true);
/// Mouse keys: the numeric keypad moves the pointer. The Shift+Alt+Num Lock
/// chord flips it.
pub const MOUSE_KEYS: crate::Key<bool> =
    crate::Key::new(crate::magnifier::SECTION, "mouse_keys", || false);
/// Whether the built-in panel stays put when a convertible is turned (the
/// Quick Settings rotation tile).
pub const ROTATION_LOCK: crate::Key<bool> =
    crate::Key::new(DISPLAY_SECTION, "rotation_lock", || false);
/// Whether the backlight follows the ambient light sensor.
pub const AUTO_BRIGHTNESS: crate::Key<bool> =
    crate::Key::new(DISPLAY_SECTION, "auto_brightness", || false);
/// Night light (the Quick Settings tile).
pub const NIGHT_LIGHT: crate::Key<bool> = crate::Key::new(DISPLAY_SECTION, "night_light", || false);

/// How a clickpad (a touchpad whose whole surface is the button) decides
/// which button a physical press means. libinput decides per device which of
/// these it supports; the compositor asks only for a supported one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchpadClickMethod {
    /// The bottom strip is split into left, middle and right button areas.
    Areas,
    /// One finger is a left click, two a right click, three a middle click,
    /// wherever they rest.
    Clickfinger,
    /// Whatever libinput picks for the device (areas on most PC clickpads,
    /// finger count on Apple-style ones). Also what an unknown value reads
    /// as, so one odd string never discards the rest of the section.
    #[default]
    #[serde(other)]
    Default,
}

impl TouchpadClickMethod {
    pub const ALL: [Self; 3] = [Self::Default, Self::Areas, Self::Clickfinger];
}

/// When a touchpad produces scrolling instead of pointer motion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchpadScrollMethod {
    /// One finger along the right or bottom edge scrolls.
    Edge,
    /// Two fingers moving together scroll; also what an unknown value reads
    /// as.
    #[default]
    #[serde(other)]
    TwoFinger,
}

impl TouchpadScrollMethod {
    pub const ALL: [Self; 2] = [Self::TwoFinger, Self::Edge];
}

/// `input` key for the scroll-distance multiplier applied to wheel and
/// finger scrolling alike.
pub const SCROLL_SPEED_KEY: &str = "scroll_speed";
/// Slowest scroll multiplier the Settings slider can reach.
pub const SCROLL_SPEED_MIN: f64 = 0.25;
/// Fastest scroll multiplier the Settings slider can reach.
pub const SCROLL_SPEED_MAX: f64 = 4.0;

/// Clamp a persisted scroll multiplier into the supported range; anything
/// non-finite is the neutral `1.0`.
pub fn clamp_scroll_speed(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(SCROLL_SPEED_MIN, SCROLL_SPEED_MAX)
    } else {
        1.0
    }
}

/// The scroll multiplier for a `0.0 … 1.0` slider position. The scale is
/// logarithmic so the midpoint is exactly `1.0` and halving feels as large a
/// step as doubling.
pub fn scroll_speed_from_slider(position: f64) -> f64 {
    let position = if position.is_finite() {
        position.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let span = SCROLL_SPEED_MAX.log2();
    clamp_scroll_speed((span * (2.0 * position - 1.0)).exp2())
}

/// The slider position for a persisted multiplier; the inverse of
/// [`scroll_speed_from_slider`].
pub fn scroll_speed_to_slider(speed: f64) -> f64 {
    let span = SCROLL_SPEED_MAX.log2();
    f64::midpoint(clamp_scroll_speed(speed).log2() / span, 1.0).clamp(0.0, 1.0)
}

fn default_scroll_speed() -> f64 {
    1.0
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent input-device toggles serialised by field name; consumers in other \
              crates read and persist them directly"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RichInputConfig {
    pub pointer_speed: f64,
    pub touchpad_pointer_speed: Option<f64>,
    pub mouse_pointer_speed: Option<f64>,
    pub touchpad_natural_scroll: bool,
    pub mouse_natural_scroll: bool,
    pub tap_to_click: bool,
    pub tap_drag: bool,
    /// Keep a tap-and-drag going after the finger lifts, until the next tap.
    pub tap_drag_lock: bool,
    pub disable_while_typing: bool,
    pub middle_emulation: bool,
    pub left_handed: bool,
    pub accel_profile: AccelProfile,
    pub key_repeat: f64,
    /// Exact physical repeat values imported from the host desktop. A Tilo
    /// repeat-slider choice clears both and resumes using `key_repeat`.
    pub key_repeat_delay_ms: Option<u32>,
    pub key_repeat_rate_hz: Option<u32>,
    /// Active source id (`layout` or `layout:variant`). Kept under the
    /// historic key so a current compositor and older Settings builds agree
    /// on the user's primary layout.
    pub keyboard_layout: String,
    /// Ordered source ids.
    pub keyboard_layouts: Vec<String>,
    /// Explicit XKB physical model, bounded to the supported PC geometries.
    pub keyboard_model: String,
    /// Scoped keyboard choices survive device removal; the default retains legacy fields.
    #[serde(deserialize_with = "crate::keyboard_sources::lenient_profiles")]
    pub keyboard_assignments: BTreeMap<String, crate::keyboard_sources::KeyboardProfile>,
    pub compose_key: Option<String>,
    /// What Caps Lock does: `None` keeps it Caps Lock; `ctrl`, `escape` or
    /// `backspace` turn it into that key.
    pub caps_lock: Option<String>,
    pub touchpad_gestures: bool,
    pub touchpad_click_method: TouchpadClickMethod,
    pub touchpad_scroll_method: TouchpadScrollMethod,
    /// Scroll-distance multiplier for wheels and touchpads, within
    /// [`SCROLL_SPEED_MIN`] … [`SCROLL_SPEED_MAX`].
    #[serde(default = "default_scroll_speed")]
    pub scroll_speed: f64,
    /// Per-tablet settings keyed by [`input_device_key`].
    #[serde(deserialize_with = "lenient_tablets")]
    pub tablets: BTreeMap<String, TabletConfig>,
}

impl Default for RichInputConfig {
    fn default() -> Self {
        Self {
            pointer_speed: 0.5,
            touchpad_pointer_speed: None,
            mouse_pointer_speed: None,
            touchpad_natural_scroll: DEFAULT_TOUCHPAD_NATURAL_SCROLL,
            mouse_natural_scroll: DEFAULT_MOUSE_NATURAL_SCROLL,
            tap_to_click: true,
            tap_drag: true,
            tap_drag_lock: false,
            disable_while_typing: true,
            middle_emulation: true,
            left_handed: false,
            accel_profile: AccelProfile::Adaptive,
            key_repeat: 0.5,
            key_repeat_delay_ms: None,
            key_repeat_rate_hz: None,
            keyboard_layout: "us".into(),
            keyboard_layouts: vec!["us".into()],
            keyboard_model: "pc105".into(),
            keyboard_assignments: BTreeMap::new(),
            compose_key: None,
            caps_lock: None,
            touchpad_gestures: true,
            touchpad_click_method: TouchpadClickMethod::Default,
            touchpad_scroll_method: TouchpadScrollMethod::TwoFinger,
            scroll_speed: default_scroll_speed(),
            tablets: BTreeMap::new(),
        }
    }
}

/// Maximum number of simultaneously compiled XKB groups. Four is XKB's
/// widely interoperable group limit (and is already more than most people
/// can switch between without losing track of the active source).
pub const MAX_KEYBOARD_INPUT_SOURCES: usize = 4;

/// Slowest repeat rate selectable by Tilo's single repeat-speed control.
/// Imported host settings may still preserve zero (repeat disabled) or a
/// higher exact rate until the person changes that control.
pub const KEY_REPEAT_SLIDER_MIN_HZ: u32 = 10;
/// Fastest repeat rate selectable by Tilo's single repeat-speed control.
pub const KEY_REPEAT_SLIDER_MAX_HZ: u32 = 50;

/// A validated XKB source from the per-user settings document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardInputSource {
    pub id: String,
    pub layout: String,
    pub variant: Option<String>,
}

impl KeyboardInputSource {
    /// Parse `layout` or `layout:variant`. XKB names come from file names and
    /// symbol section identifiers, so accepting path punctuation or commas
    /// would turn one preference into extra rule components. The compositor
    /// validates independently even though Settings only offers installed
    /// catalogue entries.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        let (layout, variant) = value
            .split_once(':')
            .map_or((value, None), |(layout, variant)| (layout, Some(variant)));
        if !valid_xkb_name(layout) || variant.is_some_and(|v| !valid_xkb_name(v)) {
            return None;
        }
        let variant = variant.map(str::to_string);
        let id = variant
            .as_deref()
            .map_or_else(|| layout.to_string(), |v| format!("{layout}:{v}"));
        Some(Self {
            id,
            layout: layout.to_string(),
            variant,
        })
    }
}

/// An input-method engine the user can pick as an input source, next to the
/// XKB layouts. Engines compose text (Chinese, Japanese, Korean) instead of
/// mapping one key to one character; the session's trusted input method
/// provides them. Only one engine owns the seat at a time, and it types on
/// the primary (first) keyboard layout of the source list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputMethodEngine {
    /// Simplified Chinese, Rime with its Pinyin schema.
    Rime,
    /// Japanese, Anthy.
    Anthy,
    /// Korean, Hangul.
    Hangul,
}

/// Source ids naming an input-method engine start with this prefix. `/` is
/// never part of an XKB name, so an engine id cannot be mistaken for a layout.
pub const INPUT_METHOD_SOURCE_PREFIX: &str = "ime/";

impl InputMethodEngine {
    pub const ALL: [Self; 3] = [Self::Rime, Self::Anthy, Self::Hangul];

    /// The engine's name in the input-method framework (and in its source id).
    pub fn name(self) -> &'static str {
        match self {
            Self::Rime => "rime",
            Self::Anthy => "anthy",
            Self::Hangul => "hangul",
        }
    }

    /// The durable source id, e.g. `ime/rime`.
    pub fn source_id(self) -> String {
        format!("{INPUT_METHOD_SOURCE_PREFIX}{}", self.name())
    }

    /// Human-readable source name for choosers and the switcher readout.
    pub fn label(self) -> &'static str {
        match self {
            Self::Rime => "Chinese (Simplified) — Rime",
            Self::Anthy => "Japanese — Anthy",
            Self::Hangul => "Korean — Hangul",
        }
    }

    /// Whether this image ships the engine, so Settings offers and the
    /// compositor starts only engines that can actually run.
    pub fn is_installed(self) -> bool {
        std::path::Path::new("/usr/share/fcitx5/inputmethod")
            .join(format!("{}.conf", self.name()))
            .is_file()
    }

    pub fn from_source_id(id: &str) -> Option<Self> {
        let name = id.trim().strip_prefix(INPUT_METHOD_SOURCE_PREFIX)?;
        Self::ALL.into_iter().find(|engine| engine.name() == name)
    }
}

/// One entry of the ordered input-source list: an XKB layout or an
/// input-method engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputSource {
    Layout(KeyboardInputSource),
    InputMethod(InputMethodEngine),
}

impl InputSource {
    pub fn parse(value: &str) -> Option<Self> {
        if let Some(engine) = InputMethodEngine::from_source_id(value) {
            return Some(Self::InputMethod(engine));
        }
        KeyboardInputSource::parse(value).map(Self::Layout)
    }

    pub fn id(&self) -> String {
        match self {
            Self::Layout(source) => source.id.clone(),
            Self::InputMethod(engine) => engine.source_id(),
        }
    }
}

fn valid_xkb_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

impl RichInputConfig {
    /// Parse the input section field by field: a missing or wrong-typed
    /// value only resets itself to its default. Old single-layout documents
    /// are then reconciled with the ordered source list, so
    /// `{"keyboard_layout":"de"}` becomes one German source instead of
    /// German plus the default US. A section that names no layout at all
    /// types the machine's own layout
    /// ([`crate::keyboard_sources::system_sources`]), not US.
    pub fn from_section(section: &serde_json::Map<String, serde_json::Value>) -> Self {
        Self::from_section_or(section, &crate::keyboard_sources::system_sources)
    }

    /// [`Self::from_section`] with the machine's layouts supplied by `system`,
    /// which is asked only when the section names none.
    fn from_section_or(
        section: &serde_json::Map<String, serde_json::Value>,
        system: &dyn Fn() -> Vec<String>,
    ) -> Self {
        let Ok(serde_json::Value::Object(mut fields)) = serde_json::to_value(Self::default())
        else {
            return Self::default();
        };
        if !section.contains_key("keyboard_layouts") {
            fields.insert("keyboard_layouts".into(), serde_json::json!([]));
        }
        for (key, value) in section {
            let Some(previous) = fields.insert(key.clone(), value.clone()) else {
                // Not one of ours (the touchpad switches, the inspector state).
                fields.remove(key);
                continue;
            };
            if serde_json::from_value::<Self>(serde_json::Value::Object(fields.clone())).is_err() {
                fields.insert(key.clone(), previous);
            }
        }
        let mut config: Self =
            serde_json::from_value(serde_json::Value::Object(fields)).unwrap_or_default();
        config.scroll_speed = clamp_scroll_speed(config.scroll_speed);
        if !matches!(
            config.keyboard_model.as_str(),
            "pc104" | "pc105" | "jp106" | "kr106"
        ) {
            config.keyboard_model = "pc105".into();
        }
        let names_layout = section
            .get("keyboard_layout")
            .is_some_and(serde_json::Value::is_string)
            || section
                .get("keyboard_layouts")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|values| values.iter().any(serde_json::Value::is_string));
        if !names_layout {
            // Nobody chose a layout: type what the machine was set up with
            // (the installer's, localed's). Passing an explicit `us` here
            // would also stop libxkbcommon applying `XKB_DEFAULT_LAYOUT`.
            config.keyboard_layouts = system();
            config.keyboard_layout = config.keyboard_layouts.first().cloned().unwrap_or_default();
        }
        config.normalise_sources_or(&|| system().into_iter().next());
        config
    }

    /// Reconcile the active source with the ordered list. A document that
    /// names no usable layout falls back to the machine's primary layout
    /// ([`crate::keyboard_sources::fallback_layout`]).
    pub fn normalise_sources(&mut self) {
        self.normalise_sources_or(&|| Some(crate::keyboard_sources::fallback_layout()));
    }

    /// [`Self::normalise_sources`] with the fallback layout supplied by
    /// `fallback`, asked only when the document names no usable layout; US
    /// when it has none either.
    fn normalise_sources_or(&mut self, fallback: &dyn Fn() -> Option<String>) {
        let fallback = || {
            fallback()
                .filter(|id| KeyboardInputSource::parse(id).is_some())
                .unwrap_or_else(|| "us".into())
        };
        let active = InputSource::parse(&self.keyboard_layout);
        let mut sources = Vec::<String>::new();
        let mut layouts = 0_usize;
        for source in &self.keyboard_layouts {
            let Some(source) = InputSource::parse(source) else {
                continue;
            };
            let id = source.id();
            if sources.contains(&id) {
                continue;
            }
            if matches!(source, InputSource::Layout(_)) {
                if layouts == MAX_KEYBOARD_INPUT_SOURCES {
                    continue;
                }
                layouts = layouts.saturating_add(1);
            }
            sources.push(id);
        }
        let active = match active {
            Some(source) if sources.contains(&source.id()) => source.id(),
            Some(InputSource::Layout(source)) => {
                if layouts == MAX_KEYBOARD_INPUT_SOURCES
                    && let Some(last) = sources
                        .iter()
                        .rposition(|id| InputMethodEngine::from_source_id(id).is_none())
                {
                    sources.remove(last);
                }
                sources.insert(0, source.id.clone());
                source.id
            }
            Some(InputSource::InputMethod(engine)) => {
                sources.push(engine.source_id());
                engine.source_id()
            }
            None => sources.first().cloned().unwrap_or_else(fallback),
        };
        // An engine types on a layout, so the list always keeps one.
        if !sources
            .iter()
            .any(|id| InputMethodEngine::from_source_id(id).is_none())
        {
            sources.insert(0, fallback());
        }
        self.keyboard_layout = active;
        self.keyboard_layouts = sources;
    }

    /// The XKB layouts of the source list, in order: the groups the keymap
    /// is compiled from. Input-method engines are not XKB groups.
    pub fn sources(&self) -> Vec<KeyboardInputSource> {
        self.keyboard_layouts
            .iter()
            .filter_map(|value| match InputSource::parse(value)? {
                InputSource::Layout(source) => Some(source),
                InputSource::InputMethod(_) => None,
            })
            .collect()
    }

    /// Make the source after the active one active, wrapping, and return
    /// its index. `None` with fewer than two sources: there is nothing to
    /// switch to.
    pub fn select_next_source(&mut self) -> Option<usize> {
        if self.keyboard_layouts.len() < 2 {
            return None;
        }
        let next = self
            .active_source_index()
            .saturating_add(1)
            .checked_rem(self.keyboard_layouts.len())?;
        self.keyboard_layout = self.keyboard_layouts.get(next)?.clone();
        Some(next)
    }

    /// Position of the active source in the full source list.
    pub fn active_source_index(&self) -> usize {
        self.keyboard_layouts
            .iter()
            .position(|id| id == &self.keyboard_layout)
            .unwrap_or(0)
    }

    /// The XKB group to select: the active layout's position among the
    /// layouts, or the primary layout (group 0) while an input-method engine
    /// is active, since the engine types on it.
    pub fn active_layout_index(&self) -> usize {
        self.sources()
            .iter()
            .position(|source| source.id == self.keyboard_layout)
            .unwrap_or(0)
    }

    /// The input-method engine that owns typing, if the active source is one.
    pub fn active_input_method(&self) -> Option<InputMethodEngine> {
        InputMethodEngine::from_source_id(&self.keyboard_layout)
    }

    /// Whether `id` may join the list: layouts are bounded by the XKB group
    /// limit, engines only by being distinct.
    pub fn can_add_source(&self, id: &str) -> bool {
        match InputSource::parse(id) {
            None => false,
            Some(source) if self.keyboard_layouts.contains(&source.id()) => false,
            Some(InputSource::InputMethod(_)) => true,
            Some(InputSource::Layout(_)) => self.sources().len() < MAX_KEYBOARD_INPUT_SOURCES,
        }
    }

    /// XKB option fragments owned by this preference section. No XKB group
    /// toggle: switching sources is the compositor's rebindable
    /// input-source shortcut, which persists the choice and shows it —
    /// an XKB toggle would switch silently, forget it on reload, and
    /// collide with Alt+Shift+Tab. A Caps Lock remap yields to Compose on
    /// Caps Lock, the more specific choice.
    pub fn xkb_options(&self) -> Vec<String> {
        let mut options = Vec::new();
        if self.compose_key.as_deref() != Some("caps")
            && let Some(option) = match self.caps_lock.as_deref() {
                Some("ctrl") => Some("ctrl:nocaps"),
                Some("escape") => Some("caps:escape"),
                Some("backspace") => Some("caps:backspace"),
                _ => None,
            }
        {
            options.push(option.into());
        }
        if let Some(option) = match self.compose_key.as_deref() {
            Some("ralt") => Some("compose:ralt"),
            Some("menu") => Some("compose:menu"),
            Some("caps") => Some("compose:caps"),
            Some("rwin") => Some("compose:rwin"),
            Some("lwin") => Some("compose:lwin"),
            _ => None,
        } {
            options.push(option.into());
        }
        options
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutAction {
    Search,
    WorkspaceOverview,
    Lock,
    QuickSettings,
    ShortcutHelp,
    FocusPreviousWindow,
    FocusNextWindow,
    PreviousTab,
    NextTab,
    /// Show the focused tab's shell context menu.
    TabMenu,
    PreviousStrip,
    NextStrip,
    PreviousWorkspace,
    NextWorkspace,
    ToggleMaximize,
    DetachTab,
    AttachTabLeft,
    AttachTabRight,
    MoveGroupLeft,
    MoveGroupRight,
    /// Carry the focused window group, whole, into the strip above.
    MoveGroupToPreviousStrip,
    /// Carry the focused window group, whole, into the strip below.
    MoveGroupToNextStrip,
    /// Move the focused window group onto the display to the left of its
    /// own (the previous one in reading order).
    MoveGroupToPreviousDisplay,
    /// Move the focused window group onto the display to the right of its
    /// own (the next one in reading order).
    MoveGroupToNextDisplay,
    ResizeNarrower,
    ResizeWider,
    /// Bring back the most recently closed tab in the current workspace.
    ReopenClosedTab,
    /// Focus the Nth window group of the current strip (Super+1 … Super+9).
    FocusGroup1,
    FocusGroup2,
    FocusGroup3,
    FocusGroup4,
    FocusGroup5,
    FocusGroup6,
    FocusGroup7,
    FocusGroup8,
    FocusGroup9,
    OpenFiles,
    OpenTerminal,
    ClipboardHistory,
    Notifications,
    ToggleFullscreen,
    ToggleMagnifier,
    MagnifierZoomIn,
    MagnifierZoomOut,
    PowerMenu,
    /// Switch to the next keyboard input source.
    NextInputSource,
    /// Open the emoji picker over the focused text field.
    EmojiPicker,
    DictationToggle,
    DictationFinish,
    DictationCancel,
}

impl ShortcutAction {
    /// The strip position a `FocusGroupN` action selects, counting from 0.
    pub fn focus_group_index(self) -> Option<usize> {
        Some(match self {
            Self::FocusGroup1 => 0,
            Self::FocusGroup2 => 1,
            Self::FocusGroup3 => 2,
            Self::FocusGroup4 => 3,
            Self::FocusGroup5 => 4,
            Self::FocusGroup6 => 5,
            Self::FocusGroup7 => 6,
            Self::FocusGroup8 => 7,
            Self::FocusGroup9 => 8,
            _ => return None,
        })
    }
}

/// Desktop-navigation preferences consumed directly by the shell.
///
/// This is deliberately separate from editable one-shot chords: Vim mode is
/// a complete navigation layer whose keys must stay internally consistent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopControlsConfig {
    /// Add Super+Ctrl+h/j/k/l and Super+Ctrl+Shift+h/j/k/l aliases for
    /// window and strip navigation and for carrying the focused group. The ordinary arrow bindings remain
    /// available.
    pub vim_mode: bool,
}

/// A stable, per-user identifier for a control that may appear in Quick
/// Settings.  The compositor deliberately persists identifiers rather than
/// a rendered grid: availability is a property of the current machine while
/// the person's ordering is durable intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuickSettingsControl {
    Wifi,
    Bluetooth,
    Vpn,
    NightLight,
    AirplaneMode,
    MobileData,
    DoNotDisturb,
    /// Stop a convertible's screen following the device. Shown only where
    /// an accelerometer exists.
    RotationLock,
}

impl QuickSettingsControl {
    /// A small, useful surface for a typical laptop.  Airplane mode and Do
    /// Not Disturb are available extras, not always-visible destinations:
    /// the compact root must still fit a small display with a VPN present.
    /// Rotation lock is hidden on machines that cannot rotate, so it costs
    /// room only on a convertible.
    pub const DEFAULT: [Self; 5] = [
        Self::Wifi,
        Self::Bluetooth,
        Self::Vpn,
        Self::NightLight,
        Self::RotationLock,
    ];

    /// Wi-Fi remains reachable even after customisation: it is the recovery
    /// path for connecting a newly installed desktop to a network.
    pub fn required(self) -> bool {
        matches!(self, Self::Wifi)
    }
}

/// Quick Settings arrangement stored in the shared settings document.
///
/// Missing, unknown and duplicate values are normalised on load.  This both
/// migrates the old fixed dashboard (which had no saved arrangement) and
/// lets later releases add controls without corrupting an older layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuickSettingsConfig {
    pub controls: Vec<QuickSettingsControl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopBarWidget {
    Clock,
    SystemMonitor,
    Calendar,
}

impl TopBarWidget {
    pub const ALL: [Self; 3] = [Self::Clock, Self::SystemMonitor, Self::Calendar];
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TopBarWidgetsConfig {
    pub widgets: Vec<TopBarWidget>,
}

impl Default for TopBarWidgetsConfig {
    fn default() -> Self {
        Self {
            widgets: TopBarWidget::ALL.to_vec(),
        }
    }
}

impl TopBarWidgetsConfig {
    #[must_use]
    pub fn normalised(mut self) -> Self {
        let mut seen = BTreeSet::new();
        self.widgets.retain(|widget| seen.insert(*widget));
        if self.widgets.is_empty() {
            self.widgets.push(TopBarWidget::Clock);
        }
        self
    }

    pub fn load() -> Self {
        serde_json::from_value::<Self>(serde_json::Value::Object(super::load_section(
            "top_bar_widgets",
        )))
        .unwrap_or_default()
        .normalised()
    }

    /// Persist the ordered top-bar widget selection.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration cannot be serialized or written.
    pub fn save(&self) -> std::io::Result<()> {
        let value =
            serde_json::to_value(self.clone().normalised()).map_err(std::io::Error::other)?;
        super::update_section(
            "top_bar_widgets",
            value.as_object().cloned().unwrap_or_default(),
        )
    }
}

impl Default for QuickSettingsConfig {
    fn default() -> Self {
        Self {
            controls: QuickSettingsControl::DEFAULT.to_vec(),
        }
    }
}

impl QuickSettingsConfig {
    #[must_use]
    pub fn normalised(mut self) -> Self {
        let mut seen = BTreeSet::new();
        self.controls.retain(|control| seen.insert(*control));
        if !self.controls.contains(&QuickSettingsControl::Wifi) {
            self.controls.insert(0, QuickSettingsControl::Wifi);
        }
        self
    }

    pub fn load() -> Self {
        Self::from_section(super::load_section("quick_settings"))
    }

    /// [`Self::load`] from an explicit settings file.
    pub fn load_at(path: &std::path::Path) -> Self {
        Self::from_section(super::load_section_at(path, "quick_settings"))
    }

    fn from_section(section: serde_json::Map<String, serde_json::Value>) -> Self {
        serde_json::from_value::<Self>(serde_json::Value::Object(section))
            .unwrap_or_default()
            .normalised()
    }

    /// # Errors
    ///
    /// Returns an error if the setting cannot be serialized or persisted.
    pub fn save(&self) -> std::io::Result<()> {
        super::update_section("quick_settings", self.section()?)
    }

    /// [`Self::save`] into an explicit settings file.
    ///
    /// # Errors
    ///
    /// Returns an error if the setting cannot be serialized or persisted.
    pub fn save_at(&self, path: &std::path::Path) -> std::io::Result<()> {
        super::update_section_at(path, "quick_settings", self.section()?)
    }

    fn section(&self) -> std::io::Result<serde_json::Map<String, serde_json::Value>> {
        let value =
            serde_json::to_value(self.clone().normalised()).map_err(std::io::Error::other)?;
        Ok(value.as_object().cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod quick_settings_tests {
    use super::{QuickSettingsConfig, QuickSettingsControl};

    #[test]
    fn quick_settings_order_dedupes_persists_and_keeps_recovery_controls() {
        // Phase `quick_settings_config_deduplicates_and_keeps_network_recovery`.
        {
            let config = QuickSettingsConfig {
                controls: vec![
                    QuickSettingsControl::NightLight,
                    QuickSettingsControl::NightLight,
                ],
            }
            .normalised();
            assert_eq!(
                config.controls,
                vec![QuickSettingsControl::Wifi, QuickSettingsControl::NightLight]
            );
        }
        // Phase `airplane_is_available_but_not_a_default_control`.
        {
            assert!(
                !QuickSettingsConfig::default()
                    .controls
                    .contains(&QuickSettingsControl::AirplaneMode)
            );
        }
        // Phase `quick_settings_order_survives_restart_without_replacing_other_settings`.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("tilo/settings.json");
            crate::update_section_at(
                &path,
                "appearance",
                serde_json::json!({"theme": "dark"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
            let expected = QuickSettingsConfig {
                controls: vec![
                    QuickSettingsControl::NightLight,
                    QuickSettingsControl::Wifi,
                    QuickSettingsControl::Bluetooth,
                ],
            };
            expected.save_at(&path).unwrap();

            assert_eq!(QuickSettingsConfig::load_at(&path), expected);
            assert_eq!(
                crate::load_at(&path)["appearance"]["theme"],
                serde_json::json!("dark")
            );
        }
    }
}

#[cfg(test)]
mod logical_extent_tests {
    use super::{OutputTransform, logical_extent};

    #[test]
    fn the_logical_extent_is_the_mode_over_the_scale_rotated() {
        assert_eq!(
            logical_extent((2560, 1600), 2.0, OutputTransform::Normal),
            (1280, 800)
        );
        assert_eq!(
            logical_extent((1920, 1200), 1.25, OutputTransform::Rotate90),
            (960, 1536)
        );
        // 2560 / 1.5 = 1706.67: nearest pixel, as zxdg_output_v1 reports.
        assert_eq!(
            logical_extent((2560, 1440), 1.5, OutputTransform::Rotate180),
            (1707, 960)
        );
        assert_eq!(
            logical_extent((1920, 1080), f64::NAN, OutputTransform::Normal),
            (1920, 1080)
        );
    }
}

#[cfg(test)]
mod top_bar_widgets_tests {
    use super::{TopBarWidget, TopBarWidgetsConfig};

    #[test]
    fn top_bar_widgets_deduplicate_and_never_become_empty() {
        let config = TopBarWidgetsConfig {
            widgets: vec![TopBarWidget::Calendar, TopBarWidget::Calendar],
        }
        .normalised();
        assert_eq!(config.widgets, vec![TopBarWidget::Calendar]);
        assert_eq!(
            TopBarWidgetsConfig { widgets: vec![] }.normalised().widgets,
            vec![TopBarWidget::Clock]
        );
    }
}

impl DesktopControlsConfig {
    pub fn load() -> Self {
        serde_json::from_value(serde_json::Value::Object(super::load_section(
            "desktop_controls",
        )))
        .unwrap_or_default()
    }

    /// # Errors
    ///
    /// Returns an error if the setting cannot be serialized or persisted.
    pub fn save(&self) -> std::io::Result<()> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        super::update_section(
            "desktop_controls",
            value.as_object().cloned().unwrap_or_default(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KeyChord {
    pub modifiers: BTreeSet<String>,
    pub key: String,
}

impl KeyChord {
    /// # Errors
    ///
    /// Returns an error if `text` names no key, its key is more than one
    /// key (contains whitespace), a modifier is unrecognised or repeated,
    /// or a single printable key has no modifier at all.
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let mut parts: Vec<_> = text
            .split('+')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        let key = parts.pop().ok_or("shortcut is empty")?;
        if key.contains(char::is_whitespace) {
            return Err("key must be one key");
        }
        let mut modifiers = BTreeSet::new();
        for part in parts {
            let canonical = match part.to_ascii_lowercase().as_str() {
                "super" | "meta" | "cmd" => "Super",
                "ctrl" | "control" => "Ctrl",
                "alt" => "Alt",
                "shift" => "Shift",
                _ => return Err("unknown modifier"),
            };
            if !modifiers.insert(canonical.into()) {
                return Err("duplicate modifier");
            }
        }
        if modifiers.is_empty() && key.len() == 1 {
            return Err("printable shortcuts need a modifier");
        }
        Ok(Self {
            modifiers,
            key: key.to_ascii_lowercase(),
        })
    }

    /// `F1` and Print Screen are compositor safety/help controls, never
    /// editable application-navigation bindings.
    pub fn is_reserved_system_key(&self) -> bool {
        matches!(self.key.as_str(), "f1" | "printscreen")
    }

    fn is_valid_configurable(&self) -> bool {
        let valid_modifiers = self
            .modifiers
            .iter()
            .all(|modifier| matches!(modifier.as_str(), "Super" | "Ctrl" | "Alt" | "Shift"));
        let printable_without_modifier = self.modifiers.is_empty() && self.key.chars().count() == 1;
        valid_modifiers
            && !self.key.is_empty()
            && !self.key.contains(char::is_whitespace)
            && !self.is_reserved_system_key()
            && !printable_without_modifier
    }

    pub fn display(&self) -> String {
        let key = match self.key.as_str() {
            "left" => "Left",
            "right" => "Right",
            "up" => "Up",
            "down" => "Down",
            "pageup" => "Page Up",
            "pagedown" => "Page Down",
            "tab" => "Tab",
            "enter" => "Enter",
            "escape" => "Escape",
            "space" => "Space",
            "delete" => "Delete",
            "f1" => "F1",
            "f10" => "F10",
            other => other,
        };
        ["Super", "Ctrl", "Alt", "Shift"]
            .into_iter()
            .filter(|modifier| self.modifiers.contains(*modifier))
            .map(str::to_string)
            .chain(std::iter::once(key.to_string()))
            .collect::<Vec<_>>()
            .join("+")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutConfig {
    pub bindings: BTreeMap<ShortcutAction, KeyChord>,
    /// Chords bound to published actions: anything the action door offers,
    /// a saved shortcut's `run` included.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionChord>,
}

/// A published action, addressed the way the action door addresses it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ActionChordTarget {
    /// The provider token (`workspace:<room>` for a workspace's own actions).
    pub provider: String,
    pub resource: String,
    pub action: String,
}

/// One chord bound to one published action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionChord {
    pub chord: KeyChord,
    pub target: ActionChordTarget,
    /// What the guide and Settings call it, e.g. "Run Publish a draft".
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutConfigError {
    ReservedSystemKey,
    InvalidChord,
    Conflict(ShortcutAction),
    /// The chord belongs to the desktop shortcut with this
    /// [`crate::keyboard_shortcuts::FixedShortcut::id`], which nothing may
    /// rebind.
    Fixed(&'static str),
    /// The chord already runs another published action.
    TakenByAction,
}

impl std::fmt::Display for ShortcutConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReservedSystemKey => {
                f.write_str("F1 and Print Screen are reserved by the desktop")
            }
            Self::InvalidChord => f.write_str("that chord is not supported"),
            Self::Conflict(action) => write!(f, "the chord is already bound to {action:?}"),
            Self::Fixed(id) => write!(f, "the chord is the desktop's fixed shortcut `{id}`"),
            Self::TakenByAction => f.write_str("the chord already runs another action"),
        }
    }
}

impl std::error::Error for ShortcutConfigError {}

impl Default for ShortcutConfig {
    #[allow(
        clippy::too_many_lines,
        reason = "the default table is one line per shortcut action"
    )]
    fn default() -> Self {
        // Built directly rather than through the fallible `KeyChord::parse`: every
        // chord here is a literal we already know is well-formed.
        fn super_chord(key: &str) -> KeyChord {
            KeyChord {
                modifiers: BTreeSet::from(["Super".to_string()]),
                key: key.to_string(),
            }
        }
        fn super_shift_chord(key: &str) -> KeyChord {
            KeyChord {
                modifiers: BTreeSet::from(["Shift".to_string(), "Super".to_string()]),
                key: key.to_string(),
            }
        }
        fn super_ctrl_shift_chord(key: &str) -> KeyChord {
            KeyChord {
                modifiers: BTreeSet::from([
                    "Ctrl".to_string(),
                    "Shift".to_string(),
                    "Super".to_string(),
                ]),
                key: key.to_string(),
            }
        }
        fn ctrl_chord(key: &str) -> KeyChord {
            KeyChord {
                modifiers: BTreeSet::from(["Ctrl".to_string()]),
                key: key.to_string(),
            }
        }
        fn ctrl_shift_chord(key: &str) -> KeyChord {
            KeyChord {
                modifiers: BTreeSet::from(["Ctrl".to_string(), "Shift".to_string()]),
                key: key.to_string(),
            }
        }
        fn ctrl_alt_chord(key: &str) -> KeyChord {
            KeyChord {
                modifiers: BTreeSet::from(["Alt".to_string(), "Ctrl".to_string()]),
                key: key.to_string(),
            }
        }
        fn super_alt_chord(key: &str) -> KeyChord {
            KeyChord {
                modifiers: BTreeSet::from(["Alt".to_string(), "Super".to_string()]),
                key: key.to_string(),
            }
        }
        let bindings = [
            (ShortcutAction::Search, super_chord("space")),
            (ShortcutAction::WorkspaceOverview, super_chord("tab")),
            (ShortcutAction::Lock, super_chord("l")),
            (ShortcutAction::QuickSettings, super_chord("q")),
            // A physical question-mark press reports the shifted slash key
            // plus Shift. Keep the stored/default chord identical to that
            // live event rather than accepting the unshifted slash instead.
            (ShortcutAction::ShortcutHelp, super_shift_chord("/")),
            // Spatial arrows are the always-visible desktop model. Alt+Tab
            // remains an immutable conventional ring alias in the shell.
            (ShortcutAction::FocusPreviousWindow, super_chord("left")),
            (ShortcutAction::FocusNextWindow, super_chord("right")),
            // Conventional application tab cycling belongs to the focused
            // Tilo window group too. Keeping this distinct from Super+Tab
            // means the overview is never opened while changing tabs.
            (ShortcutAction::PreviousTab, ctrl_shift_chord("tab")),
            (ShortcutAction::NextTab, ctrl_chord("tab")),
            (ShortcutAction::TabMenu, super_chord("f10")),
            // Arrows move focus across the plane; Shift carries the focused
            // group the same way (docs/architecture/window-management.md).
            (ShortcutAction::PreviousStrip, super_chord("up")),
            (ShortcutAction::NextStrip, super_chord("down")),
            (ShortcutAction::PreviousWorkspace, super_chord("pageup")),
            (ShortcutAction::NextWorkspace, super_chord("pagedown")),
            (ShortcutAction::ToggleMaximize, super_chord("m")),
            (ShortcutAction::DetachTab, super_shift_chord("enter")),
            (ShortcutAction::AttachTabLeft, super_chord("[")),
            (ShortcutAction::AttachTabRight, super_chord("]")),
            (ShortcutAction::MoveGroupLeft, super_shift_chord("left")),
            (ShortcutAction::MoveGroupRight, super_shift_chord("right")),
            (
                ShortcutAction::MoveGroupToPreviousStrip,
                super_shift_chord("up"),
            ),
            (
                ShortcutAction::MoveGroupToNextStrip,
                super_shift_chord("down"),
            ),
            // Super+Shift+arrows already reorder within a display; Ctrl
            // widens the same gesture to the neighbouring display.
            (
                ShortcutAction::MoveGroupToPreviousDisplay,
                super_ctrl_shift_chord("left"),
            ),
            (
                ShortcutAction::MoveGroupToNextDisplay,
                super_ctrl_shift_chord("right"),
            ),
            (ShortcutAction::ResizeNarrower, super_chord("-")),
            (ShortcutAction::ResizeWider, super_chord("=")),
            // Restore the most recently closed frame in this workspace.
            // Plain Super+T keeps adding a tab.
            (ShortcutAction::ReopenClosedTab, ctrl_shift_chord("t")),
            (ShortcutAction::FocusGroup1, super_chord("1")),
            (ShortcutAction::FocusGroup2, super_chord("2")),
            (ShortcutAction::FocusGroup3, super_chord("3")),
            (ShortcutAction::FocusGroup4, super_chord("4")),
            (ShortcutAction::FocusGroup5, super_chord("5")),
            (ShortcutAction::FocusGroup6, super_chord("6")),
            (ShortcutAction::FocusGroup7, super_chord("7")),
            (ShortcutAction::FocusGroup8, super_chord("8")),
            (ShortcutAction::FocusGroup9, super_chord("9")),
            (ShortcutAction::OpenFiles, super_chord("e")),
            (ShortcutAction::OpenTerminal, ctrl_alt_chord("t")),
            (ShortcutAction::ClipboardHistory, super_chord("v")),
            // Super+N already opens an app as an adjacent window, so the
            // notification list takes Windows' other Action Center key.
            (ShortcutAction::Notifications, super_chord("a")),
            (ShortcutAction::ToggleFullscreen, super_chord("f")),
            // GNOME's and Windows' magnifier keys (Super+Alt+8, Super+Plus)
            // — the second is already the resize-wider default.
            (ShortcutAction::ToggleMagnifier, super_alt_chord("8")),
            // GNOME's zoom keys; Super+= and Super+- already resize windows.
            (ShortcutAction::MagnifierZoomIn, super_alt_chord("=")),
            (ShortcutAction::MagnifierZoomOut, super_alt_chord("-")),
            // The session action sheet: the chord every PC user tries when
            // something has hung.
            (ShortcutAction::PowerMenu, ctrl_alt_chord("delete")),
            // Super+Space is global search; the Alt-modified chord is the
            // nearest free spelling of the GNOME/Windows convention.
            (ShortcutAction::NextInputSource, super_alt_chord("space")),
            // Windows' emoji panel key; GNOME uses Super+. for the same.
            (ShortcutAction::EmojiPicker, super_chord(".")),
            (ShortcutAction::DictationToggle, super_shift_chord("d")),
            (ShortcutAction::DictationFinish, super_alt_chord("d")),
            (ShortcutAction::DictationCancel, super_shift_chord("escape")),
        ]
        .into_iter()
        .collect();
        Self {
            bindings,
            actions: Vec::new(),
        }
    }
}

/// Chords that used to be an action's default, as `(action, modifiers in
/// sorted order, key)`. Never remove an entry: a settings file written by
/// any earlier release may still carry it.
const RETIRED_DEFAULTS: &[(ShortcutAction, &[&str], &str)] = &[
    (ShortcutAction::ReopenClosedTab, &["Shift", "Super"], "t"),
    (ShortcutAction::PreviousStrip, &["Shift", "Super"], "up"),
    (ShortcutAction::NextStrip, &["Shift", "Super"], "down"),
    (ShortcutAction::ToggleMaximize, &["Super"], "up"),
    (ShortcutAction::Search, &["Super"], "k"),
    (ShortcutAction::ReopenClosedTab, &["Shift", "Super"], "t"),
];

impl ShortcutConfig {
    /// Validate an already assembled table before it becomes live runtime
    /// state.  Unlike [`Self::load`], this rejects rather than repairs so a
    /// caller cannot install ambiguous ownership in a running session.
    /// # Errors
    ///
    /// Returns an error when a chord is invalid or reserved, or when two
    /// actions claim the same chord.
    pub fn validate(&self) -> Result<(), ShortcutConfigError> {
        let mut seen = BTreeMap::<&KeyChord, ShortcutAction>::new();
        for (action, chord) in &self.bindings {
            if chord.is_reserved_system_key() {
                return Err(ShortcutConfigError::ReservedSystemKey);
            }
            if !chord.is_valid_configurable() {
                return Err(ShortcutConfigError::InvalidChord);
            }
            if let Some(other) = seen.insert(chord, *action) {
                return Err(ShortcutConfigError::Conflict(other));
            }
        }
        for (index, bound) in self.actions.iter().enumerate() {
            self.check_action_chord(&bound.chord, Some(&bound.target))?;
            if self
                .actions
                .iter()
                .skip(index.saturating_add(1))
                .any(|other| other.target == bound.target)
            {
                return Err(ShortcutConfigError::TakenByAction);
            }
        }
        Ok(())
    }

    /// Whether `chord` may run the published action `target`: a valid,
    /// unreserved chord that is neither one of the shell's fixed chords, nor
    /// bound to a configurable desktop action, nor running another action.
    ///
    /// # Errors
    ///
    /// Why the chord cannot be bound.
    pub fn check_action_chord(
        &self,
        chord: &KeyChord,
        target: Option<&ActionChordTarget>,
    ) -> Result<(), ShortcutConfigError> {
        if chord.is_reserved_system_key() {
            return Err(ShortcutConfigError::ReservedSystemKey);
        }
        if !chord.is_valid_configurable() {
            return Err(ShortcutConfigError::InvalidChord);
        }
        if let Some(fixed) = crate::keyboard_shortcuts::desktop_owner(chord) {
            return Err(ShortcutConfigError::Fixed(fixed.id));
        }
        if let Some((action, _)) = self.bindings.iter().find(|(_, bound)| *bound == chord) {
            return Err(ShortcutConfigError::Conflict(*action));
        }
        if self
            .actions
            .iter()
            .any(|bound| bound.chord == *chord && Some(&bound.target) != target)
        {
            return Err(ShortcutConfigError::TakenByAction);
        }
        Ok(())
    }

    /// Bind `chord` to the published action `target`, replacing the chord
    /// it had.
    ///
    /// # Errors
    ///
    /// Why the chord cannot be bound; the table is then unchanged.
    pub fn set_action(
        &mut self,
        chord: KeyChord,
        target: ActionChordTarget,
        label: impl Into<String>,
    ) -> Result<(), ShortcutConfigError> {
        self.check_action_chord(&chord, Some(&target))?;
        self.actions.retain(|bound| bound.target != target);
        self.actions.push(ActionChord {
            chord,
            target,
            label: label.into(),
        });
        Ok(())
    }

    /// Unbind whatever chord runs `target`. `false` when none did.
    pub fn remove_action(&mut self, target: &ActionChordTarget) -> bool {
        let before = self.actions.len();
        self.actions.retain(|bound| bound.target != *target);
        self.actions.len() != before
    }

    /// The action `chord` runs, when it runs one.
    #[must_use]
    pub fn action_for(&self, chord: &KeyChord) -> Option<&ActionChord> {
        self.actions.iter().find(|bound| bound.chord == *chord)
    }

    /// Move `action` to `chord`. A chord that already belongs to someone
    /// else is refused, naming the holder, rather than taken from it.
    ///
    /// # Errors
    ///
    /// Returns an error without changing this binding when `chord` is invalid,
    /// system-reserved, or already belongs to another action, to a fixed
    /// desktop shortcut (other than one `action` shares by default), or to a
    /// published action.
    pub fn set(
        &mut self,
        action: ShortcutAction,
        chord: KeyChord,
    ) -> Result<(), ShortcutConfigError> {
        let defaults = Self::default();
        if chord.is_reserved_system_key() {
            return Err(ShortcutConfigError::ReservedSystemKey);
        }
        if !chord.is_valid_configurable() {
            return Err(ShortcutConfigError::InvalidChord);
        }
        if let Some((other, _)) = self
            .bindings
            .iter()
            .find(|(a, c)| **a != action && **c == chord)
        {
            return Err(ShortcutConfigError::Conflict(*other));
        }
        // Some defaults are also fixed chords (Super+Page Up changes the
        // workspace either way); an action may always return to its own.
        if defaults.bindings.get(&action) != Some(&chord)
            && let Some(fixed) = crate::keyboard_shortcuts::desktop_owner(&chord)
        {
            return Err(ShortcutConfigError::Fixed(fixed.id));
        }
        if self.actions.iter().any(|bound| bound.chord == chord) {
            return Err(ShortcutConfigError::TakenByAction);
        }
        self.bindings.insert(action, chord);
        Ok(())
    }
    pub fn load() -> Self {
        Self::from_section(super::load_section("shortcuts"))
    }

    /// [`Self::load`] from an explicit settings file.
    pub fn load_at(path: &std::path::Path) -> Self {
        Self::from_section(super::load_section_at(path, "shortcuts"))
    }

    /// The configuration a `shortcuts` section holds, repaired as
    /// [`Self::normalise`] repairs it.
    pub fn from_section(section: serde_json::Map<String, serde_json::Value>) -> Self {
        let loaded: Self =
            serde_json::from_value(serde_json::Value::Object(section)).unwrap_or_default();
        Self::normalise(loaded)
    }

    /// Deterministically repairs manual/corrupt persisted state: begin with
    /// defaults, then accept a stored override only when it is a valid,
    /// non-reserved, non-conflicting chord.  Bad entries fall back to their
    /// default instead of leaving ambiguous client ownership.
    ///
    /// A stored chord that was once the default for its action
    /// ([`RETIRED_DEFAULTS`]) was saved because [`Self::save`] writes the
    /// whole table, not because the person chose it; it yields to the
    /// current default.
    #[must_use]
    pub fn normalise(loaded: Self) -> Self {
        let defaults = Self::default();
        let mut effective = defaults.clone();
        for (action, chord) in loaded.bindings {
            if RETIRED_DEFAULTS.iter().any(|(retired, modifiers, key)| {
                *retired == action
                    && chord.key == *key
                    && chord
                        .modifiers
                        .iter()
                        .map(String::as_str)
                        .eq(modifiers.iter().copied())
            }) {
                continue;
            }
            if !chord.is_valid_configurable() {
                continue;
            }
            effective.bindings.remove(&action);
            if effective
                .bindings
                .values()
                .any(|existing| existing == &chord)
            {
                if let Some(default) = defaults.bindings.get(&action) {
                    effective.bindings.insert(action, default.clone());
                }
                continue;
            }
            effective.bindings.insert(action, chord);
        }
        for bound in loaded.actions {
            // The same rule as a live edit: an action chord never takes a
            // desktop chord, and the first binding of a chord or target wins.
            if effective
                .actions
                .iter()
                .all(|other| other.target != bound.target)
            {
                let _ = effective.set_action(bound.chord, bound.target, bound.label);
            }
        }
        effective
    }
    /// # Errors
    ///
    /// Returns an error if `self` can't be serialised or the section can't
    /// be written.
    pub fn save(&self) -> std::io::Result<()> {
        super::update_section("shortcuts", self.section()?)
    }

    /// [`Self::save`] into an explicit settings file.
    ///
    /// # Errors
    ///
    /// Returns an error if `self` can't be serialised or the section can't
    /// be written.
    pub fn save_at(&self, path: &std::path::Path) -> std::io::Result<()> {
        super::update_section_at(path, "shortcuts", self.section()?)
    }

    /// The `shortcuts` section holding this configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if `self` can't be serialised.
    pub fn section(&self) -> std::io::Result<serde_json::Map<String, serde_json::Value>> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        Ok(value.as_object().cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn display(name: &str) -> OutputDescriptor {
        OutputDescriptor {
            connector: name.into(),
            connector_label: name.into(),
            modes: vec![OutputMode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            }],
            hdr: false,
            vrr: false,
            color_profiles: vec![],
            monitor: None,
        }
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn display_layouts_reject_unadvertised_modes_and_invalid_mirrors() {
        // Phase `rejects_modes_the_connector_does_not_advertise`.
        {
            let mut cfg = DisplayConfig::default();
            cfg.outputs.insert(
                "DP-1".into(),
                OutputConfig {
                    mode: Some(OutputMode {
                        width: 3840,
                        height: 2160,
                        refresh_mhz: 60_000,
                    }),
                    ..Default::default()
                },
            );
            assert!(matches!(
                cfg.validate_for(&[display("DP-1")]),
                Err(DisplayConfigError::UnsupportedMode { .. })
            ));
        }
        // Phase `legacy_unqualified_connector_never_cross_targets_qualified_duplicates`.
        {
            let first = OutputDescriptor {
                connector: "pci:0000:01:00.0/HDMI-A-1".into(),
                connector_label: "HDMI-A-1".into(),
                modes: vec![],
                hdr: false,
                vrr: false,
                color_profiles: vec![],
                monitor: None,
            };
            let second = OutputDescriptor {
                connector: "pci:0000:03:00.0/HDMI-A-1".into(),
                connector_label: "HDMI-A-1".into(),
                modes: vec![],
                hdr: false,
                vrr: false,
                color_profiles: vec![],
                monitor: None,
            };
            let mut config = DisplayConfig::default();
            config.outputs.insert(
                "HDMI-A-1".into(),
                OutputConfig {
                    enabled: false,
                    ..Default::default()
                },
            );

            assert!(config.output_for(&first.connector).is_none());
            assert!(config.output_for(&second.connector).is_none());
            assert_eq!(first.label(), "HDMI-A-1");
            assert_eq!(first.card_qualifier(), Some("pci:0000:01:00.0"));

            // Snapshots written before `connector_label` existed remain readable
            // and continue to present the historical raw connector label.
            let legacy: OutputDescriptor = serde_json::from_value(serde_json::json!({
                "connector": "HDMI-A-1",
                "modes": [], "hdr": false, "vrr": false, "color_profiles": []
            }))
            .expect("old snapshot remains compatible");
            assert_eq!(legacy.label(), "HDMI-A-1");
            assert_eq!(legacy.card_qualifier(), None);
        }
        // Phase `rejects_mirror_cycles`.
        {
            let mut cfg = DisplayConfig::default();
            cfg.outputs.insert(
                "DP-1".into(),
                OutputConfig {
                    mirror_of: Some("DP-2".into()),
                    ..Default::default()
                },
            );
            cfg.outputs.insert(
                "DP-2".into(),
                OutputConfig {
                    mirror_of: Some("DP-1".into()),
                    ..Default::default()
                },
            );
            assert!(matches!(
                cfg.validate_for(&[display("DP-1"), display("DP-2")]),
                Err(DisplayConfigError::MirrorCycle { .. })
            ));
        }
        // Phase `rejects_mirror_pair_with_different_explicit_modes`.
        {
            let mut left = display("DP-1");
            left.modes.push(OutputMode {
                width: 1280,
                height: 720,
                refresh_mhz: 60_000,
            });
            let right = display("DP-2");
            let mut cfg = DisplayConfig::default();
            cfg.outputs.insert(
                "DP-1".into(),
                OutputConfig {
                    mode: Some(left.modes[1]),
                    mirror_of: Some("DP-2".into()),
                    ..Default::default()
                },
            );
            cfg.outputs.insert(
                "DP-2".into(),
                OutputConfig {
                    mode: Some(right.modes[0]),
                    ..Default::default()
                },
            );
            assert!(matches!(
                cfg.validate_for(&[left, right]),
                Err(DisplayConfigError::IncompatibleMirrorModes { .. })
            ));
        }
        // Phase `mirror_pair_accepts_same_resolution_at_different_refresh_rates`.
        {
            let mut panel = display("eDP-1");
            panel.modes[0].refresh_mhz = 60_000;
            let mut usb_c = display("DP-1");
            usb_c.modes[0].refresh_mhz = 59_940;
            let mut cfg = DisplayConfig::default();
            cfg.outputs.insert(
                "eDP-1".into(),
                OutputConfig {
                    primary: true,
                    mode: Some(panel.modes[0]),
                    ..Default::default()
                },
            );
            cfg.outputs.insert(
                "DP-1".into(),
                OutputConfig {
                    mode: Some(usb_c.modes[0]),
                    mirror_of: Some("eDP-1".into()),
                    ..Default::default()
                },
            );

            assert_eq!(cfg.validate_for(&[panel, usb_c]), Ok(()));
        }
    }

    #[test]
    fn a_monitor_is_named_by_brand_and_model_once() {
        let named = |make: &str, model: &str| {
            MonitorIdentity {
                make: make.into(),
                model: model.into(),
                ..MonitorIdentity::default()
            }
            .display_name()
        };
        assert_eq!(named("Dell", "DELL U2720Q"), "Dell U2720Q");
        assert_eq!(named("LG", "27GL850"), "LG 27GL850");
        assert_eq!(named("", "Q27G2"), "Q27G2");
        assert_eq!(named("BOE", ""), "BOE");
    }

    #[test]
    fn display_confirmations_are_precise_transitions_without_phantom_requests() {
        // Phase `display_confirmation_expiry_is_a_precise_state_transition`.
        {
            let mut transaction = DisplayTransaction {
                state: DisplayTransactionState::AwaitingConfirmation,
                deadline_unix_ms: Some(25_000),
                ..Default::default()
            };
            assert!(!transaction.expired(24_999));
            assert!(transaction.expired(25_000));
            transaction.state = DisplayTransactionState::Confirmed;
            assert!(!transaction.expired(u64::MAX));
        }
        // Phase `missing_transaction_section_does_not_create_a_phantom_request`.
        {
            assert!(DisplayTransaction::from_section(serde_json::Map::new()).is_none());
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn process_start_identity_rejects_pid_reuse_shape() {
        let current = process_start_ticks(std::process::id())
            .expect("the current Linux process has /proc stat data");
        assert!(current > 0);
        let stale = DisplayHardwareSnapshot {
            generated_unix_ms: 0,
            backend: "test".into(),
            publisher_pid: std::process::id(),
            publisher_start_ticks: current.saturating_add(1),
            ..DisplayHardwareSnapshot::default()
        };
        assert_ne!(
            process_start_ticks(stale.publisher_pid),
            Some(stale.publisher_start_ticks)
        );
    }

    #[test]
    fn reopen_closed_tab_defaults_to_ctrl_shift_t_without_colliding() {
        let cfg = ShortcutConfig::default();
        assert_eq!(
            cfg.bindings[&ShortcutAction::ReopenClosedTab],
            KeyChord::parse("Ctrl+Shift+T").unwrap()
        );
        assert!(cfg.validate().is_ok(), "no other default claims the chord");
    }

    #[test]
    fn saved_reopen_default_migrates_without_overwriting_a_custom_chord() {
        let mut loaded = ShortcutConfig::default();
        loaded.bindings.insert(
            ShortcutAction::ReopenClosedTab,
            KeyChord::parse("Super+Shift+T").unwrap(),
        );
        assert_eq!(
            ShortcutConfig::normalise(loaded.clone()).bindings[&ShortcutAction::ReopenClosedTab],
            KeyChord::parse("Ctrl+Shift+T").unwrap()
        );
        loaded.bindings.insert(
            ShortcutAction::ReopenClosedTab,
            KeyChord::parse("Alt+Shift+T").unwrap(),
        );
        assert_eq!(
            ShortcutConfig::normalise(loaded).bindings[&ShortcutAction::ReopenClosedTab],
            KeyChord::parse("Alt+Shift+T").unwrap()
        );
    }

    #[test]
    fn reopening_migrates_the_old_default_and_preserves_custom_bindings() {
        let action = ShortcutAction::ReopenClosedTab;
        let mut old = ShortcutConfig::default();
        old.bindings
            .insert(action, KeyChord::parse("Super+Shift+T").unwrap());
        let migrated = ShortcutConfig::normalise(old);
        assert_eq!(
            migrated.bindings[&action],
            KeyChord::parse("Ctrl+Shift+T").unwrap()
        );
        assert!(migrated.validate().is_ok());
        let custom = KeyChord::parse("Ctrl+Alt+Shift+T").unwrap();
        let mut edited = ShortcutConfig::default();
        edited.set(action, custom.clone()).unwrap();
        assert_eq!(ShortcutConfig::normalise(edited).bindings[&action], custom);
    }

    #[test]
    fn standard_desktop_chords_have_their_conventional_defaults() {
        let cfg = ShortcutConfig::default();
        let shown = |action| cfg.bindings[&action].display();
        for (n, action) in [
            ShortcutAction::FocusGroup1,
            ShortcutAction::FocusGroup2,
            ShortcutAction::FocusGroup3,
            ShortcutAction::FocusGroup4,
            ShortcutAction::FocusGroup5,
            ShortcutAction::FocusGroup6,
            ShortcutAction::FocusGroup7,
            ShortcutAction::FocusGroup8,
            ShortcutAction::FocusGroup9,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(shown(action), format!("Super+{}", n + 1));
            assert_eq!(action.focus_group_index(), Some(n));
        }
        assert_eq!(shown(ShortcutAction::OpenFiles), "Super+e");
        assert_eq!(shown(ShortcutAction::OpenTerminal), "Ctrl+Alt+t");
        assert_eq!(shown(ShortcutAction::ClipboardHistory), "Super+v");
        assert_eq!(shown(ShortcutAction::Notifications), "Super+a");
        assert_eq!(shown(ShortcutAction::ToggleFullscreen), "Super+f");
        assert_eq!(shown(ShortcutAction::ToggleMagnifier), "Super+Alt+8");
        assert_eq!(shown(ShortcutAction::MagnifierZoomIn), "Super+Alt+=");
        assert_eq!(shown(ShortcutAction::MagnifierZoomOut), "Super+Alt+-");
        assert_eq!(shown(ShortcutAction::PowerMenu), "Ctrl+Alt+Delete");
        assert_eq!(shown(ShortcutAction::EmojiPicker), "Super+.");
        assert_eq!(shown(ShortcutAction::TabMenu), "Super+F10");
        assert_eq!(ShortcutAction::OpenFiles.focus_group_index(), None);
        assert!(cfg.validate().is_ok(), "no two defaults share a chord");
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn unavailable_linux_process_identity_is_not_invented() {
        assert_eq!(process_start_ticks(std::process::id()), None);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn shortcuts_are_normalised_conflict_free_and_reload_unchanged() {
        // Phase `shortcut_parser_normalises_and_rejects_plain_letters`.
        {
            assert_eq!(KeyChord::parse("meta + K").unwrap().display(), "Super+k");
            assert!(KeyChord::parse("k").is_err());
        }
        // Phase `window_management_defaults_are_coherent_and_conflict_free`.
        {
            let cfg = ShortcutConfig::default();
            assert_eq!(
                cfg.bindings[&ShortcutAction::ToggleMaximize].display(),
                "Super+m"
            );
            for (action, chord) in [
                (ShortcutAction::FocusPreviousWindow, "Super+Left"),
                (ShortcutAction::FocusNextWindow, "Super+Right"),
                (ShortcutAction::PreviousStrip, "Super+Up"),
                (ShortcutAction::NextStrip, "Super+Down"),
                (ShortcutAction::MoveGroupLeft, "Super+Shift+Left"),
                (ShortcutAction::MoveGroupRight, "Super+Shift+Right"),
                (ShortcutAction::MoveGroupToPreviousStrip, "Super+Shift+Up"),
                (ShortcutAction::MoveGroupToNextStrip, "Super+Shift+Down"),
            ] {
                assert_eq!(
                    cfg.bindings[&action].display(),
                    chord,
                    "arrows move focus and Shift carries the group the same way"
                );
            }
            assert_eq!(
                cfg.bindings[&ShortcutAction::PreviousWorkspace].display(),
                "Super+Page Up"
            );
            assert_eq!(
                cfg.bindings[&ShortcutAction::NextWorkspace].display(),
                "Super+Page Down"
            );
            assert_eq!(
                cfg.bindings[&ShortcutAction::DetachTab].display(),
                "Super+Shift+Enter"
            );
            assert_eq!(
                cfg.bindings[&ShortcutAction::AttachTabLeft].display(),
                "Super+["
            );
            assert_eq!(
                cfg.bindings[&ShortcutAction::AttachTabRight].display(),
                "Super+]"
            );
            assert!(cfg.validate().is_ok());
        }
        // Phase `shortcut_conflicts_are_rejected_without_overwriting`.
        {
            let mut cfg = ShortcutConfig::default();
            let old = cfg.bindings[&ShortcutAction::Lock].clone();
            assert_eq!(
                cfg.set(
                    ShortcutAction::Lock,
                    KeyChord::parse("Super+Space").unwrap()
                ),
                Err(ShortcutConfigError::Conflict(ShortcutAction::Search))
            );
            assert_eq!(cfg.bindings[&ShortcutAction::Lock], old);
        }
        // Phase `reserved_and_duplicate_shortcuts_are_rejected_at_the_public_setter`.
        {
            let mut cfg = ShortcutConfig::default();
            for key in ["f1", "printscreen"] {
                assert_eq!(
                    cfg.set(
                        ShortcutAction::Search,
                        KeyChord {
                            modifiers: BTreeSet::from(["Ctrl".to_string()]),
                            key: key.into(),
                        },
                    ),
                    Err(ShortcutConfigError::ReservedSystemKey),
                    "{key} must remain compositor-owned"
                );
            }
            cfg.set(ShortcutAction::Search, KeyChord::parse("Ctrl+J").unwrap())
                .unwrap();
            assert_eq!(
                cfg.set(ShortcutAction::NextTab, KeyChord::parse("Ctrl+J").unwrap()),
                Err(ShortcutConfigError::Conflict(ShortcutAction::Search))
            );
        }
        // Phase `corrupt_persisted_shortcuts_normalise_without_ambiguous_ownership`.
        {
            let mut corrupt = ShortcutConfig::default();
            corrupt.bindings.insert(
                ShortcutAction::Search,
                KeyChord {
                    modifiers: BTreeSet::from(["Ctrl".to_string()]),
                    key: "f1".into(),
                },
            );
            corrupt
                .bindings
                .insert(ShortcutAction::Lock, KeyChord::parse("Alt+L").unwrap());
            corrupt.bindings.insert(
                ShortcutAction::PreviousTab,
                KeyChord::parse("Ctrl+J").unwrap(),
            );
            corrupt
                .bindings
                .insert(ShortcutAction::NextTab, KeyChord::parse("Ctrl+J").unwrap());

            let repaired = ShortcutConfig::normalise(corrupt);
            assert_eq!(
                repaired.bindings[&ShortcutAction::Search].display(),
                "Super+Space",
                "reserved persisted keys revert to their action default"
            );
            assert_eq!(
                repaired.bindings[&ShortcutAction::Lock].display(),
                "Alt+l",
                "the lock chord is rebindable like every other action"
            );
            assert_eq!(
                repaired.bindings[&ShortcutAction::PreviousTab].display(),
                "Ctrl+j",
                "the first action in stable action order owns a duplicate"
            );
            assert_eq!(
                repaired.bindings[&ShortcutAction::NextTab].display(),
                "Ctrl+Tab",
                "the duplicate action deterministically falls back to default"
            );
            assert!(repaired.validate().is_ok());
        }
        // Phase `a_table_saved_under_retired_defaults_takes_the_new_ones`.
        {
            let mut saved_by_an_older_release = ShortcutConfig::default();
            for action in [
                ShortcutAction::MoveGroupToPreviousStrip,
                ShortcutAction::MoveGroupToNextStrip,
            ] {
                saved_by_an_older_release.bindings.remove(&action);
            }
            for (action, chord) in [
                (ShortcutAction::PreviousStrip, "Super+Shift+Up"),
                (ShortcutAction::NextStrip, "Super+Shift+Down"),
                (ShortcutAction::ToggleMaximize, "Super+Up"),
            ] {
                saved_by_an_older_release
                    .bindings
                    .insert(action, KeyChord::parse(chord).unwrap());
            }
            assert_eq!(
                ShortcutConfig::normalise(saved_by_an_older_release.clone()),
                ShortcutConfig::default(),
                "an untouched older table is today's defaults"
            );

            saved_by_an_older_release.bindings.insert(
                ShortcutAction::NextStrip,
                KeyChord::parse("Super+J").unwrap(),
            );
            let repaired = ShortcutConfig::normalise(saved_by_an_older_release);
            assert_eq!(
                repaired.bindings[&ShortcutAction::NextStrip].display(),
                "Super+j",
                "a chord the person chose is kept"
            );
            assert!(repaired.validate().is_ok());
        }
        // Phase `saved_shortcuts_reload_as_the_same_valid_effective_config`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("tilo/settings.json");

            let mut saved = ShortcutConfig::default();
            saved
                .set(ShortcutAction::NextTab, KeyChord::parse("Ctrl+J").unwrap())
                .unwrap();
            saved.save_at(&path).unwrap();
            let reloaded = ShortcutConfig::load_at(&path);

            assert_eq!(reloaded, saved);
            assert!(reloaded.validate().is_ok());
        }
        // Phase `desktop_navigation_defaults_are_complete_and_conflict_free`.
        {
            let cfg = ShortcutConfig::default();
            assert_eq!(cfg.bindings.len(), 50);
            let unique: BTreeSet<_> = cfg.bindings.values().collect();
            assert_eq!(unique.len(), cfg.bindings.len());
            assert_eq!(
                cfg.bindings[&ShortcutAction::FocusPreviousWindow].display(),
                "Super+Left"
            );
            assert_eq!(
                cfg.bindings[&ShortcutAction::FocusNextWindow].display(),
                "Super+Right"
            );
            assert_eq!(
                cfg.bindings[&ShortcutAction::PreviousTab].display(),
                "Ctrl+Shift+Tab"
            );
            assert_eq!(cfg.bindings[&ShortcutAction::NextTab].display(), "Ctrl+Tab");
            // Reordering within a display keeps Super+Shift+arrows; moving to
            // the neighbouring display adds Ctrl to the same gesture.
            assert_eq!(
                cfg.bindings[&ShortcutAction::MoveGroupToNextDisplay].display(),
                "Super+Ctrl+Shift+Right"
            );
        }
    }
    #[test]
    fn a_chord_runs_any_published_action_but_never_takes_a_desktop_chord() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tilo/settings.json");
        let target = |resource: &str| ActionChordTarget {
            provider: "workspace:room".into(),
            resource: resource.into(),
            action: "run".into(),
        };
        let mut cfg = ShortcutConfig::default();
        let chord = KeyChord::parse("Super+Shift+N").unwrap();
        cfg.set_action(chord.clone(), target("shortcuts.shortcut:a"), "Run A")
            .unwrap();
        assert_eq!(
            cfg.action_for(&chord).map(|bound| bound.label.as_str()),
            Some("Run A")
        );
        assert_eq!(
            cfg.set_action(chord.clone(), target("shortcuts.shortcut:b"), "Run B"),
            Err(ShortcutConfigError::TakenByAction),
            "one chord runs one action"
        );
        assert_eq!(
            cfg.set_action(
                KeyChord::parse("Alt+Tab").unwrap(),
                target("shortcuts.shortcut:b"),
                "Run B"
            ),
            Err(ShortcutConfigError::Fixed("fixed-switch-windows"))
        );
        assert_eq!(
            cfg.set_action(
                KeyChord::parse("Super+Space").unwrap(),
                target("shortcuts.shortcut:b"),
                "Run B"
            ),
            Err(ShortcutConfigError::Conflict(ShortcutAction::Search))
        );
        assert_eq!(
            cfg.set(ShortcutAction::Search, chord.clone()),
            Err(ShortcutConfigError::TakenByAction),
            "a desktop action cannot take an action's chord either"
        );
        cfg.save_at(&path).unwrap();
        let reloaded = ShortcutConfig::load_at(&path);
        assert_eq!(reloaded, cfg);
        assert!(reloaded.validate().is_ok());
        assert!(cfg.remove_action(&target("shortcuts.shortcut:a")));
        assert!(cfg.action_for(&chord).is_none());
    }

    #[test]
    fn input_sources_and_pointer_settings_migrate_validate_and_survive_damage() {
        // Phase `legacy_single_keyboard_layout_migrates_into_sources`.
        {
            let section = serde_json::json!({ "keyboard_layout": "de" })
                .as_object()
                .unwrap()
                .clone();
            let cfg = RichInputConfig::from_section(&section);
            assert_eq!(cfg.keyboard_layout, "de");
            assert_eq!(cfg.keyboard_layouts, ["de"]);
            assert_eq!(cfg.active_source_index(), 0);
        }
        // Phase `an_unset_layout_follows_the_machine_and_a_chosen_one_wins`.
        {
            let machine = || vec!["de".to_string(), "us".to_string()];
            let unset = RichInputConfig::from_section_or(&serde_json::Map::new(), &machine);
            assert_eq!(unset.keyboard_layouts, ["de", "us"]);
            assert_eq!(unset.keyboard_layout, "de");
            let chosen = serde_json::json!({ "keyboard_layout": "fr" })
                .as_object()
                .unwrap()
                .clone();
            let chosen = RichInputConfig::from_section_or(&chosen, &|| {
                panic!("a chosen layout never asks the machine")
            });
            assert_eq!(chosen.keyboard_layouts, ["fr"]);
            let bare = RichInputConfig::from_section_or(&serde_json::Map::new(), &Vec::new);
            assert_eq!(bare.keyboard_layouts, ["us"], "no layout anywhere: US");
            // A damaged document falls back to the machine, not to US.
            let damaged = serde_json::json!({ "keyboard_layout": "../evil" })
                .as_object()
                .unwrap()
                .clone();
            let damaged = RichInputConfig::from_section_or(&damaged, &machine);
            assert_eq!(damaged.keyboard_layouts, ["de"]);
            assert_eq!(damaged.keyboard_layout, "de");
            let engine_only = serde_json::json!({
                "keyboard_layout": "ime/rime",
                "keyboard_layouts": ["ime/rime"]
            })
            .as_object()
            .unwrap()
            .clone();
            let engine_only = RichInputConfig::from_section_or(&engine_only, &machine);
            assert_eq!(engine_only.keyboard_layouts, ["de", "ime/rime"]);
        }
        // Phase `scroll_defaults_match_gnome`.
        {
            let empty = RichInputConfig::from_section(&serde_json::Map::new());
            assert!(empty.touchpad_natural_scroll);
            assert!(!empty.mouse_natural_scroll);
        }
        // Phase `keyboard_sources_are_validated_deduplicated_and_bounded`.
        {
            let section = serde_json::json!({
                "keyboard_layout": "de:nodeadkeys",
                "keyboard_layouts": [
                    "us", "de:nodeadkeys", "us", "../evil", "fr", "es", "gb"
                ]
            })
            .as_object()
            .unwrap()
            .clone();
            let cfg = RichInputConfig::from_section(&section);
            assert_eq!(cfg.keyboard_layouts.len(), MAX_KEYBOARD_INPUT_SOURCES);
            assert_eq!(cfg.keyboard_layouts, ["us", "de:nodeadkeys", "fr", "es"]);
            assert_eq!(cfg.active_source_index(), 1);
            assert_eq!(cfg.sources()[1].variant.as_deref(), Some("nodeadkeys"));
        }
        // Phase `keyboard_options_do_not_steal_the_shell_search_shortcut`.
        {
            let mut cfg = RichInputConfig {
                keyboard_layouts: vec!["us".into(), "de".into()],
                compose_key: Some("ralt".into()),
                ..Default::default()
            };
            cfg.normalise_sources();
            assert_eq!(
                cfg.xkb_options(),
                ["compose:ralt"],
                "no XKB group toggle: it collides with Alt+Shift+Tab and switches silently"
            );
            assert_eq!(
                ShortcutConfig::default().bindings[&ShortcutAction::NextInputSource].display(),
                "Super+Alt+Space"
            );
        }
        // Phase `malformed_pointer_field_does_not_discard_keyboard_sources`.
        {
            let section = serde_json::json!({
                "pointer_speed": "fast",
                "keyboard_layout": "de",
                "keyboard_layouts": ["us", "de"]
            })
            .as_object()
            .unwrap()
            .clone();
            let cfg = RichInputConfig::from_section(&section);
            assert_eq!(cfg.keyboard_layouts, ["us", "de"]);
            assert_eq!(cfg.active_source_index(), 1);
        }
    }

    #[test]
    fn touchpad_click_and_scroll_choices_round_trip_through_the_section() {
        let config = RichInputConfig {
            touchpad_click_method: TouchpadClickMethod::Clickfinger,
            touchpad_scroll_method: TouchpadScrollMethod::Edge,
            scroll_speed: 2.0,
            ..Default::default()
        };
        let section = serde_json::to_value(&config)
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(section["touchpad_click_method"], "clickfinger");
        assert_eq!(section["touchpad_scroll_method"], "edge");
        let back = RichInputConfig::from_section(&section);
        assert_eq!(back.touchpad_click_method, TouchpadClickMethod::Clickfinger);
        assert_eq!(back.touchpad_scroll_method, TouchpadScrollMethod::Edge);
        assert!((back.scroll_speed - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn publisher_liveness_is_its_held_lease_not_its_proc_entry() {
        let dir = tempfile::tempdir().unwrap();
        // No process has this PID, so /proc cannot vouch for it, as it cannot
        // for a non-dumpable compositor. Holding its lease can.
        let (pid, start_ticks) = (4_000_000, 7);
        let lease_path = publisher_lease_path(dir.path(), pid, start_ticks);
        assert!(
            !publisher_live_in(dir.path(), pid, start_ticks),
            "no lease yet"
        );
        let lease = std::fs::File::create(&lease_path).unwrap();
        assert!(
            !publisher_live_in(dir.path(), pid, start_ticks),
            "a lease file its publisher no longer locks is a dead publisher's"
        );
        let lease = advisory_lock::FileLock::try_exclusive(lease).unwrap();
        assert!(publisher_live_in(dir.path(), pid, start_ticks));
        assert!(!publisher_live_in(dir.path(), pid, start_ticks + 1));
        assert!(!publisher_live_in(dir.path(), 0, start_ticks));
        drop(lease);
        assert!(
            !publisher_live_in(dir.path(), pid, start_ticks),
            "the kernel drops the lease with its holder"
        );

        let (own_pid, own_ticks) = publisher_identity_in(dir.path()).unwrap();
        assert_eq!(own_pid, std::process::id());
        assert!(publisher_live_in(dir.path(), own_pid, own_ticks));
        assert_eq!(
            publisher_identity_in(dir.path()).unwrap(),
            (own_pid, own_ticks),
            "publishing again keeps the one lease"
        );
    }

    #[test]
    fn input_snapshot_is_trusted_only_from_its_live_publisher() {
        let dir = std::env::temp_dir().join(format!("tilo-input-state-{}", std::process::id()));
        let path = dir.join("input-state.json");
        let pen = InputDeviceSnapshot {
            key: input_device_key(0x056a, 0x0374, "Wacom Intuos S Pen"),
            name: "Wacom Intuos S Pen".into(),
            kind: InputDeviceKind::Tablet,
            ..InputDeviceSnapshot::default()
        };
        assert_eq!(pen.key, "056a:0374:Wacom Intuos S Pen");
        let (publisher_pid, publisher_start_ticks) =
            publisher_identity_in(&dir).expect("hold a publisher lease");
        let live = InputHardwareSnapshot {
            publisher_pid,
            publisher_start_ticks,
            devices: vec![pen.clone(), pen],
            active_keyboard: None,
        };
        live.save_at(&path).unwrap();
        let back = InputHardwareSnapshot::load_at(&path).expect("live publisher");
        assert!(back.has(InputDeviceKind::Tablet));
        assert!(!back.has(InputDeviceKind::Touchpad));
        assert_eq!(
            back.of_kind(InputDeviceKind::Tablet).len(),
            1,
            "one section per identity, however many handles libinput opened"
        );

        let stale = InputHardwareSnapshot {
            publisher_start_ticks: live.publisher_start_ticks.wrapping_add(1),
            ..live
        };
        stale.save_at(&path).unwrap();
        assert!(
            InputHardwareSnapshot::load_at(&path).is_none(),
            "a reused PID must not resurrect a dead compositor's devices"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "presets are exact constants handed back unchanged"
    )]
    fn tablet_settings_round_trip_per_device_and_survive_a_bad_neighbour() {
        let mut pen = TabletConfig {
            output: Some("DP-2".into()),
            keep_aspect: true,
            left_handed: true,
            pressure_curve: pressure_curve_for_feel(0.0),
            lower_button: StylusButtonAction::Right,
            upper_button: StylusButtonAction::Shortcut(ShortcutAction::Search),
            ..TabletConfig::default()
        };
        pen.pad_buttons.insert(3, ShortcutAction::WorkspaceOverview);
        pen.pad_map_button = Some(1);
        let key = input_device_key(0x056a, 0x0374, "Wacom Intuos S Pen");
        let section = serde_json::json!({
            TABLETS_KEY: {
                key.clone(): serde_json::to_value(&pen).unwrap(),
                "0000:0000:Broken": {"keep_aspect": "sometimes"},
            }
        })
        .as_object()
        .unwrap()
        .clone();
        let value = serde_json::to_value(&pen).unwrap();
        assert_eq!(
            value["upper_button"],
            serde_json::json!({"shortcut": "search"})
        );
        assert_eq!(value["lower_button"], "right");
        assert_eq!(value["pad_buttons"]["3"], "workspace_overview");
        assert_eq!(value["pad_map_button"], 1);

        let tablets = tablets_from_section(&section);
        assert_eq!(tablets.get(&key), Some(&pen));
        assert_eq!(tablets.len(), 1, "a malformed entry is skipped, not fatal");

        let rich = RichInputConfig::from_section(&section);
        assert_eq!(rich.tablets.get(&key), Some(&pen));

        let fresh = TabletConfig::default();
        assert_eq!(fresh.output, None, "a new tablet spans the whole desktop");
        assert_eq!(fresh.pressure_curve, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "presets are exact constants handed back unchanged"
    )]
    fn tip_feel_positions_and_curves_map_both_ways() {
        assert_eq!(pressure_curve_for_feel(0.5), PRESSURE_CURVE_PRESETS[3]);
        assert_eq!(pressure_curve_for_feel(0.0), PRESSURE_CURVE_PRESETS[0]);
        assert_eq!(pressure_curve_for_feel(1.0), PRESSURE_CURVE_PRESETS[6]);
        assert_eq!(pressure_curve_for_feel(f64::NAN), PRESSURE_CURVE_PRESETS[3]);
        for (index, preset) in PRESSURE_CURVE_PRESETS.iter().enumerate() {
            let feel = feel_for_pressure_curve(*preset);
            assert_eq!(pressure_curve_for_feel(feel), *preset, "preset {index}");
        }
        assert!(
            (feel_for_pressure_curve([0.02, 0.73, 0.26, 0.98]) - 0.0).abs() < f64::EPSILON,
            "a hand-edited curve reads as its nearest preset"
        );
    }

    #[test]
    fn pad_dials_keep_a_choice_per_mode_and_the_eraser_its_own() {
        let mut pad = TabletConfig::default();
        assert_eq!(
            pad.dial_action(PadDial::Ring(0), 0),
            PadDialAction::Scroll,
            "an untouched ring scrolls"
        );
        pad.set_dial_action(PadDial::Ring(0), 2, PadDialAction::BrushSize);
        pad.set_dial_action(PadDial::Strip(1), 0, PadDialAction::Volume);
        assert_eq!(
            pad.dial_action(PadDial::Ring(0), 2),
            PadDialAction::BrushSize
        );
        assert_eq!(pad.dial_action(PadDial::Ring(0), 1), PadDialAction::Scroll);
        assert_eq!(pad.dial_action(PadDial::Ring(0), 7), PadDialAction::Scroll);
        assert_eq!(pad.dial_action(PadDial::Ring(1), 2), PadDialAction::Scroll);
        assert_eq!(pad.dial_action(PadDial::Strip(1), 0), PadDialAction::Volume);
        assert_eq!(
            pad.dial_action(PadDial::Strip(0), 0),
            PadDialAction::Scroll,
            "rings and strips are numbered apart"
        );

        let value = serde_json::to_value(&pad).unwrap();
        assert_eq!(
            value["pad_rings"]["0"],
            serde_json::json!(["scroll", "scroll", "brush_size"])
        );
        assert_eq!(value["pad_strips"]["1"], serde_json::json!(["volume"]));
        assert_eq!(value["eraser"], "native");
        let back: TabletConfig = serde_json::from_value(value).unwrap();
        assert_eq!(back, pad);

        pad.set_dial_action(PadDial::Ring(0), 2, PadDialAction::Scroll);
        assert!(
            pad.pad_rings.is_empty(),
            "a ring back on its defaults stores nothing"
        );

        let pen = TabletConfig {
            eraser: EraserAction::Shortcut(ShortcutAction::Search),
            ..TabletConfig::default()
        };
        assert_eq!(
            serde_json::to_value(&pen).unwrap()["eraser"],
            serde_json::json!({"shortcut": "search"})
        );
    }

    #[test]
    fn unknown_touchpad_choices_reset_only_themselves() {
        let section = serde_json::json!({
            "touchpad_click_method": "three_finger_magic",
            "touchpad_scroll_method": "sideways",
            "scroll_speed": 99.0,
            "tap_to_click": false
        })
        .as_object()
        .unwrap()
        .clone();
        let cfg = RichInputConfig::from_section(&section);
        assert_eq!(cfg.touchpad_click_method, TouchpadClickMethod::Default);
        assert_eq!(cfg.touchpad_scroll_method, TouchpadScrollMethod::TwoFinger);
        assert!((cfg.scroll_speed - SCROLL_SPEED_MAX).abs() < f64::EPSILON);
        assert!(!cfg.tap_to_click, "the rest of the section survives");
    }

    #[test]
    fn scroll_slider_midpoint_is_neutral_and_the_mapping_inverts() {
        assert!((scroll_speed_from_slider(0.5) - 1.0).abs() < 1e-12);
        assert!((scroll_speed_from_slider(0.0) - SCROLL_SPEED_MIN).abs() < 1e-12);
        assert!((scroll_speed_from_slider(1.0) - SCROLL_SPEED_MAX).abs() < 1e-12);
        for position in [0.0, 0.1, 0.25, 0.5, 0.8, 1.0] {
            let back = scroll_speed_to_slider(scroll_speed_from_slider(position));
            assert!((back - position).abs() < 1e-9, "{position} -> {back}");
        }
        assert!((clamp_scroll_speed(f64::NAN) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn input_method_engines_share_the_source_list_without_taking_xkb_groups() {
        let section = serde_json::json!({
            "keyboard_layout": "ime/rime",
            "keyboard_layouts": [
                "us", "ime/rime", "de", "ime/rime", "ime/mozc", "fr", "es", "gb", "ime/anthy"
            ]
        })
        .as_object()
        .unwrap()
        .clone();
        let cfg = RichInputConfig::from_section(&section);
        assert_eq!(
            cfg.keyboard_layouts,
            ["us", "ime/rime", "de", "fr", "es", "ime/anthy"],
            "engines do not count against the four XKB groups; unknown engines are dropped"
        );
        assert_eq!(
            cfg.sources()
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["us", "de", "fr", "es"]
        );
        assert_eq!(cfg.active_source_index(), 1);
        assert_eq!(cfg.active_input_method(), Some(InputMethodEngine::Rime));
        assert_eq!(
            cfg.active_layout_index(),
            0,
            "an engine types on the primary layout"
        );
        assert!(cfg.xkb_options().is_empty());
    }

    #[test]
    fn the_next_source_wraps_through_layouts_and_engines() {
        let mut cfg = RichInputConfig {
            keyboard_layout: "de".into(),
            keyboard_layouts: vec!["us".into(), "de".into(), "fr".into()],
            ..Default::default()
        };
        assert_eq!(cfg.select_next_source(), Some(2));
        assert_eq!(cfg.keyboard_layout, "fr");
        assert_eq!(cfg.select_next_source(), Some(0));
        assert_eq!(cfg.keyboard_layout, "us");
        let mut single = RichInputConfig::default();
        assert_eq!(single.select_next_source(), None);
        assert_eq!(single.keyboard_layout, "us");
        let mut with_engine = RichInputConfig {
            keyboard_layout: "us".into(),
            keyboard_layouts: vec!["us".into(), "ime/rime".into()],
            ..Default::default()
        };
        assert_eq!(with_engine.select_next_source(), Some(1));
        assert_eq!(
            with_engine.active_input_method(),
            Some(InputMethodEngine::Rime)
        );
    }

    #[test]
    fn caps_lock_remaps_map_to_standard_xkb_options() {
        for (choice, option) in [
            ("ctrl", "ctrl:nocaps"),
            ("escape", "caps:escape"),
            ("backspace", "caps:backspace"),
        ] {
            let section = serde_json::json!({ "caps_lock": choice })
                .as_object()
                .unwrap()
                .clone();
            assert_eq!(
                RichInputConfig::from_section(&section).xkb_options(),
                [option]
            );
        }
        let composing = RichInputConfig {
            caps_lock: Some("escape".into()),
            compose_key: Some("caps".into()),
            ..Default::default()
        };
        assert_eq!(
            composing.xkb_options(),
            ["compose:caps"],
            "Caps Lock cannot be both Compose and Escape"
        );
        assert!(RichInputConfig::default().xkb_options().is_empty());
    }

    #[test]
    fn an_active_layout_maps_to_its_group_past_interleaved_engines() {
        let mut cfg = RichInputConfig {
            keyboard_layout: "de".into(),
            keyboard_layouts: vec!["us".into(), "ime/hangul".into(), "de".into()],
            ..Default::default()
        };
        cfg.normalise_sources();
        assert_eq!(cfg.active_source_index(), 2);
        assert_eq!(cfg.active_layout_index(), 1);
        assert_eq!(cfg.active_input_method(), None);
    }

    #[test]
    fn an_engine_only_list_keeps_a_layout_to_type_on() {
        let mut cfg = RichInputConfig {
            keyboard_layout: "ime/anthy".into(),
            keyboard_layouts: vec!["ime/anthy".into()],
            ..Default::default()
        };
        cfg.normalise_sources();
        assert_eq!(cfg.keyboard_layouts, ["us", "ime/anthy"]);
        assert_eq!(cfg.keyboard_layout, "ime/anthy");
        assert!(
            cfg.xkb_options().is_empty(),
            "one layout needs no group toggle"
        );
    }

    #[test]
    fn engine_ids_never_parse_as_xkb_layouts() {
        assert!(KeyboardInputSource::parse("ime/rime").is_none());
        assert_eq!(
            InputSource::parse("ime/hangul"),
            Some(InputSource::InputMethod(InputMethodEngine::Hangul))
        );
        assert!(InputSource::parse("ime/../rime").is_none());
        for engine in InputMethodEngine::ALL {
            assert_eq!(
                InputMethodEngine::from_source_id(&engine.source_id()),
                Some(engine)
            );
        }
    }

    #[test]
    fn adding_sources_bounds_layouts_but_not_engines() {
        let mut cfg = RichInputConfig {
            keyboard_layouts: vec!["us".into(), "de".into(), "fr".into(), "es".into()],
            ..Default::default()
        };
        cfg.normalise_sources();
        assert!(!cfg.can_add_source("gb"));
        assert!(cfg.can_add_source("ime/rime"));
        assert!(!cfg.can_add_source("de"), "already present");
        assert!(!cfg.can_add_source("ime/nope"));
    }
}
