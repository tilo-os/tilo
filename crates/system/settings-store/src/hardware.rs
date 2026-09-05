//! Typed, validated compositor-facing hardware preferences.
//!
//! Settings is not allowed to invent capabilities: callers pass a live
//! [`OutputDescriptor`] to [`DisplayConfig::validate_for`] and only the
//! resulting plan may be applied.  The file is durable intent; DRM/libinput
//! remain the authority for what the currently attached hardware can do.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputTransform {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputMode {
    pub width: u32,
    pub height: u32,
    /// Milli-Hz keeps 59.94 and 60 Hz distinct.
    pub refresh_mhz: u32,
}

impl OutputMode {
    pub fn label(self) -> String {
        format!(
            "{} × {} · {:.2} Hz",
            self.width,
            self.height,
            f64::from(self.refresh_mhz) / 1000.0
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputDescriptor {
    /// Connector name exposed by DRM, e.g. `eDP-1` or `HDMI-A-1`.
    pub connector: String,
    pub modes: Vec<OutputMode>,
    pub hdr: bool,
    pub vrr: bool,
    pub color_profiles: Vec<String>,
}

/// One compositor-published output capability snapshot.  Settings consumes
/// this instead of probing DRM itself: the process that owns KMS is the only
/// authority that can say which modes and output features are actually
/// usable by the active renderer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayOutputSnapshot {
    pub descriptor: OutputDescriptor,
    pub current_mode: OutputMode,
    pub builtin: bool,
    /// The sink/EDID may support HDR even when the active scanout pipeline
    /// cannot safely signal or encode it. `descriptor.hdr` is the usable
    /// capability; this field explains the distinction without creating a
    /// decorative toggle.
    pub sink_hdr: bool,
    pub hdr_unavailable_reason: Option<String>,
    pub sink_vrr: bool,
    pub vrr_unavailable_reason: Option<String>,
    /// Stable profile identifier (currently the canonical ICC path) to a
    /// user-facing label. Only profiles in this map may be selected.
    pub color_profile_labels: BTreeMap<String, String>,
}

/// Read-only state written atomically by the compositor in the private user
/// runtime directory. It is deliberately separate from durable intent:
/// unplugging a monitor changes this document, not the user's saved layout.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayHardwareSnapshot {
    pub generated_unix_ms: u64,
    pub backend: String,
    pub publisher_pid: u32,
    /// Linux `/proc/<pid>/stat` start-time ticks. Paired with the PID so a
    /// stale runtime file cannot become trusted after PID reuse.
    pub publisher_start_ticks: u64,
    pub outputs: Vec<DisplayOutputSnapshot>,
}

impl DisplayHardwareSnapshot {
    pub fn load() -> Option<Self> {
        let path = display_snapshot_path()?;
        let snapshot: Self = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        let live_start = process_start_ticks(snapshot.publisher_pid)?;
        (snapshot.publisher_pid != 0 && live_start == snapshot.publisher_start_ticks)
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
        atomic_file::write_creating_parent(&path, &body)
    }
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayTransaction {
    pub id: u64,
    pub state: DisplayTransactionState,
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
    pub fn request(candidate: DisplayConfig) -> std::io::Result<Self> {
        let now = unix_ms();
        let transaction = Self {
            id: now,
            state: DisplayTransactionState::Requested,
            previous: DisplayConfig::load(),
            candidate,
            deadline_unix_ms: None,
            message: None,
        };
        transaction.save()?;
        Ok(transaction)
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
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("qos/display-state.json"))
}

pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
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
    pub vrr: bool,
    pub color_profile: Option<String>,
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
            vrr: false,
            color_profile: None,
        }
    }
}

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
    UnsupportedVrr { output: String },
    UnsupportedColorProfile { output: String },
}

impl DisplayConfig {
    /// Validate intent against a fresh hardware snapshot. Unknown/disconnected
    /// configured outputs are retained for re-plug but ignored in this plan.
    ///
    /// # Errors
    ///
    /// Returns the specific [`DisplayConfigError`] variant naming the first
    /// rule an enabled, attached output's configuration violates — no
    /// enabled output, more than one primary, an unsupported mode, scale,
    /// HDR, VRR or color profile, or an unresolved or cyclic mirror target.
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
            if cfg.vrr && !desc.vrr {
                return Err(DisplayConfigError::UnsupportedVrr {
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
                let compatible = match (cfg.mode, target_mode) {
                    (Some(source), Some(target)) => source == target,
                    _ => desc
                        .modes
                        .iter()
                        .any(|mode| target_desc.modes.contains(mode)),
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

    pub fn load() -> Self {
        serde_json::from_value(serde_json::Value::Object(super::load_section(
            "display_outputs",
        )))
        .unwrap_or_default()
    }

    /// Configuration the compositor should apply right now. A staged
    /// transaction temporarily overrides committed intent while the user can
    /// still see and confirm the result; rejection/revert always resolves to
    /// the last known-good configuration.
    pub fn load_effective() -> Self {
        let committed = Self::load();
        let Some(transaction) = DisplayTransaction::load() else {
            return committed;
        };
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

    /// # Errors
    ///
    /// Returns an error if `self` can't be serialised or the section can't
    /// be written.
    pub fn save(&self) -> std::io::Result<()> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        super::update_section(
            "display_outputs",
            value.as_object().cloned().unwrap_or_default(),
        )
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
    /// Legacy fallback for settings written before mouse/touchpad scrolling
    /// were independently configurable.
    pub natural_scroll: bool,
    pub touchpad_natural_scroll: Option<bool>,
    pub mouse_natural_scroll: Option<bool>,
    pub tap_to_click: bool,
    pub tap_drag: bool,
    pub disable_while_typing: bool,
    pub middle_emulation: bool,
    pub left_handed: bool,
    pub accel_profile: AccelProfile,
    pub key_repeat: f64,
    /// Active source id (`layout` or `layout:variant`). Kept under the
    /// historic key so a current compositor and older Settings builds agree
    /// on the user's primary layout.
    pub keyboard_layout: String,
    /// Ordered source ids. `layouts` is accepted as an alias for the brief
    /// development version that used that spelling.
    #[serde(alias = "layouts")]
    pub keyboard_layouts: Vec<String>,
    pub compose_key: Option<String>,
    pub touchpad_gestures: bool,
    pub pen_output: Option<String>,
}

impl Default for RichInputConfig {
    fn default() -> Self {
        Self {
            pointer_speed: 0.5,
            touchpad_pointer_speed: None,
            mouse_pointer_speed: None,
            natural_scroll: false,
            touchpad_natural_scroll: None,
            mouse_natural_scroll: None,
            tap_to_click: true,
            tap_drag: true,
            disable_while_typing: true,
            middle_emulation: true,
            left_handed: false,
            accel_profile: AccelProfile::Adaptive,
            key_repeat: 0.5,
            keyboard_layout: "us".into(),
            keyboard_layouts: vec!["us".into()],
            compose_key: None,
            touchpad_gestures: true,
            pen_output: None,
        }
    }
}

/// Maximum number of simultaneously compiled XKB groups. Four is XKB's
/// widely interoperable group limit (and is already more than most people
/// can switch between without losing track of the active source).
pub const MAX_KEYBOARD_INPUT_SOURCES: usize = 4;

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

fn valid_xkb_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

impl RichInputConfig {
    /// Parse the input section using field-by-field defaults, then reconcile
    /// old single-layout documents with the ordered source list. Invalid and
    /// duplicate ids are ignored; the active legacy source is inserted when
    /// it is absent, so `{"keyboard_layout":"de"}` migrates to one German
    /// source instead of silently falling back to US.
    pub fn from_section(section: &serde_json::Map<String, serde_json::Value>) -> Self {
        let mut config: Self =
            serde_json::from_value(serde_json::Value::Object(section.clone())).unwrap_or_default();
        // Read the keyboard fields independently as well: a wrong-typed
        // pointer field must not discard an otherwise valid input-source
        // list merely because serde rejected the whole rich object.
        config.keyboard_layout = section
            .get("keyboard_layout")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("us")
            .to_string();
        config.keyboard_layouts = section
            .get("keyboard_layouts")
            .or_else(|| section.get("layouts"))
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        config.compose_key = section
            .get("compose_key")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        config.normalise_sources();
        config
    }

    pub fn normalise_sources(&mut self) {
        // The fallback is `KeyboardInputSource::parse("us")`'s output written out by hand,
        // rather than re-parsing a literal we already know is valid XKB.
        let active = KeyboardInputSource::parse(&self.keyboard_layout).unwrap_or_else(|| {
            KeyboardInputSource {
                id: "us".to_string(),
                layout: "us".to_string(),
                variant: None,
            }
        });
        let mut sources = Vec::<String>::new();
        for source in &self.keyboard_layouts {
            let Some(source) = KeyboardInputSource::parse(source) else {
                continue;
            };
            if !sources.contains(&source.id) {
                sources.push(source.id);
            }
            if sources.len() == MAX_KEYBOARD_INPUT_SOURCES {
                break;
            }
        }
        if !sources.contains(&active.id) {
            if sources.len() == MAX_KEYBOARD_INPUT_SOURCES {
                sources.pop();
            }
            sources.insert(0, active.id.clone());
        }
        if sources.is_empty() {
            sources.push("us".into());
        }
        self.keyboard_layout = active.id;
        self.keyboard_layouts = sources;
    }

    pub fn sources(&self) -> Vec<KeyboardInputSource> {
        self.keyboard_layouts
            .iter()
            .filter_map(|value| KeyboardInputSource::parse(value))
            .collect()
    }

    pub fn active_source_index(&self) -> usize {
        self.keyboard_layouts
            .iter()
            .position(|id| id == &self.keyboard_layout)
            .unwrap_or(0)
    }

    /// XKB option fragments owned by this preference section. Layout
    /// switching is Alt+Shift because Super+Space is qOS global search;
    /// Compose remains independently configurable.
    pub fn xkb_options(&self) -> Vec<String> {
        let mut options = Vec::new();
        if self.keyboard_layouts.len() > 1 {
            options.push("grp:alt_shift_toggle".into());
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
    PreviousStrip,
    NextStrip,
    PreviousWorkspace,
    NextWorkspace,
    MoveGroupLeft,
    MoveGroupRight,
    ResizeNarrower,
    ResizeWider,
}

/// Desktop-navigation preferences consumed directly by the shell.
///
/// This is deliberately separate from editable one-shot chords: Vim mode is
/// a complete navigation layer whose keys must stay internally consistent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopControlsConfig {
    /// Add Super+Ctrl+h/j/k/l and Super+Ctrl+Shift+h/l aliases for window,
    /// strip and reorder navigation. The ordinary arrow bindings remain
    /// available.
    pub vim_mode: bool,
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

    pub fn display(&self) -> String {
        let key = match self.key.as_str() {
            "left" => "Left",
            "right" => "Right",
            "up" => "Up",
            "down" => "Down",
            "tab" => "Tab",
            "enter" => "Enter",
            "escape" => "Escape",
            "space" => "Space",
            "f1" => "F1",
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
}

impl Default for ShortcutConfig {
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
        fn alt_chord(key: &str) -> KeyChord {
            KeyChord {
                modifiers: BTreeSet::from(["Alt".to_string()]),
                key: key.to_string(),
            }
        }
        fn alt_shift_chord(key: &str) -> KeyChord {
            KeyChord {
                modifiers: BTreeSet::from(["Alt".to_string(), "Shift".to_string()]),
                key: key.to_string(),
            }
        }
        let bindings = [
            (ShortcutAction::Search, super_chord("k")),
            (ShortcutAction::WorkspaceOverview, super_chord("tab")),
            (ShortcutAction::Lock, super_chord("l")),
            (ShortcutAction::QuickSettings, super_chord("q")),
            (ShortcutAction::ShortcutHelp, super_chord("/")),
            (ShortcutAction::FocusPreviousWindow, alt_shift_chord("tab")),
            (ShortcutAction::FocusNextWindow, alt_chord("tab")),
            (ShortcutAction::PreviousTab, ctrl_shift_chord("tab")),
            (ShortcutAction::NextTab, ctrl_chord("tab")),
            (ShortcutAction::PreviousStrip, super_shift_chord("up")),
            (ShortcutAction::NextStrip, super_shift_chord("down")),
            (ShortcutAction::PreviousWorkspace, super_chord("up")),
            (ShortcutAction::NextWorkspace, super_chord("down")),
            (ShortcutAction::MoveGroupLeft, super_shift_chord("left")),
            (ShortcutAction::MoveGroupRight, super_shift_chord("right")),
            (ShortcutAction::ResizeNarrower, super_chord("-")),
            (ShortcutAction::ResizeWider, super_chord("=")),
        ]
        .into_iter()
        .collect();
        Self { bindings }
    }
}

impl ShortcutConfig {
    /// # Errors
    ///
    /// Returns the other [`ShortcutAction`] already bound to `chord`, if
    /// any, without changing this binding's own chord.
    pub fn set(&mut self, action: ShortcutAction, chord: KeyChord) -> Result<(), ShortcutAction> {
        if let Some((other, _)) = self
            .bindings
            .iter()
            .find(|(a, c)| **a != action && **c == chord)
        {
            return Err(*other);
        }
        self.bindings.insert(action, chord);
        Ok(())
    }
    pub fn load() -> Self {
        let mut loaded: Self =
            serde_json::from_value(serde_json::Value::Object(super::load_section("shortcuts")))
                .unwrap_or_default();
        // Settings written by an older release contain a valid but partial
        // map. Merge newly introduced actions without overwriting any chord
        // the user already chose.
        for (action, chord) in Self::default().bindings {
            loaded.bindings.entry(action).or_insert(chord);
        }
        loaded
    }
    /// # Errors
    ///
    /// Returns an error if `self` can't be serialised or the section can't
    /// be written.
    pub fn save(&self) -> std::io::Result<()> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        super::update_section("shortcuts", value.as_object().cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn display(name: &str) -> OutputDescriptor {
        OutputDescriptor {
            connector: name.into(),
            modes: vec![OutputMode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            }],
            hdr: false,
            vrr: false,
            color_profiles: vec![],
        }
    }
    #[test]
    fn rejects_modes_the_connector_does_not_advertise() {
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
    #[test]
    fn rejects_mirror_cycles() {
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
    #[test]
    fn rejects_mirror_pair_with_different_explicit_modes() {
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

    #[test]
    fn display_confirmation_expiry_is_a_precise_state_transition() {
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

    #[test]
    fn missing_transaction_section_does_not_create_a_phantom_request() {
        assert!(DisplayTransaction::from_section(serde_json::Map::new()).is_none());
    }

    #[test]
    fn process_start_identity_rejects_pid_reuse_shape() {
        let current = process_start_ticks(std::process::id())
            .expect("the current Linux process has /proc stat data");
        assert!(current > 0);
        let stale = DisplayHardwareSnapshot {
            generated_unix_ms: 0,
            backend: "test".into(),
            publisher_pid: std::process::id(),
            publisher_start_ticks: current.saturating_add(1),
            outputs: Vec::new(),
        };
        assert_ne!(
            process_start_ticks(stale.publisher_pid),
            Some(stale.publisher_start_ticks)
        );
    }
    #[test]
    fn shortcut_parser_normalises_and_rejects_plain_letters() {
        assert_eq!(KeyChord::parse("meta + K").unwrap().display(), "Super+k");
        assert!(KeyChord::parse("k").is_err());
    }
    #[test]
    fn shortcut_conflicts_are_rejected_without_overwriting() {
        let mut cfg = ShortcutConfig::default();
        let old = cfg.bindings[&ShortcutAction::Lock].clone();
        assert_eq!(
            cfg.set(ShortcutAction::Lock, KeyChord::parse("Super+K").unwrap()),
            Err(ShortcutAction::Search)
        );
        assert_eq!(cfg.bindings[&ShortcutAction::Lock], old);
    }

    #[test]
    fn desktop_navigation_defaults_are_complete_and_conflict_free() {
        let cfg = ShortcutConfig::default();
        assert_eq!(cfg.bindings.len(), 17);
        let unique: BTreeSet<_> = cfg.bindings.values().collect();
        assert_eq!(unique.len(), cfg.bindings.len());
        assert_eq!(
            cfg.bindings[&ShortcutAction::FocusPreviousWindow].display(),
            "Alt+Shift+Tab"
        );
        assert_eq!(
            cfg.bindings[&ShortcutAction::FocusNextWindow].display(),
            "Alt+Tab"
        );
        assert_eq!(
            cfg.bindings[&ShortcutAction::PreviousTab].display(),
            "Ctrl+Shift+Tab"
        );
        assert_eq!(cfg.bindings[&ShortcutAction::NextTab].display(), "Ctrl+Tab");
    }

    #[test]
    fn legacy_single_keyboard_layout_migrates_into_sources() {
        let section = serde_json::json!({ "keyboard_layout": "de" })
            .as_object()
            .unwrap()
            .clone();
        let cfg = RichInputConfig::from_section(&section);
        assert_eq!(cfg.keyboard_layout, "de");
        assert_eq!(cfg.keyboard_layouts, ["de"]);
        assert_eq!(cfg.active_source_index(), 0);
    }

    #[test]
    fn keyboard_sources_are_validated_deduplicated_and_bounded() {
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

    #[test]
    fn keyboard_options_do_not_steal_the_shell_search_shortcut() {
        let mut cfg = RichInputConfig {
            keyboard_layouts: vec!["us".into(), "de".into()],
            compose_key: Some("ralt".into()),
            ..Default::default()
        };
        cfg.normalise_sources();
        assert_eq!(cfg.xkb_options(), ["grp:alt_shift_toggle", "compose:ralt"]);
    }

    #[test]
    fn malformed_pointer_field_does_not_discard_keyboard_sources() {
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
