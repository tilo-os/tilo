//! `QuickSettings` — the top-right status-area popover.
//!
//! Level 1 owns the two headline controls (brightness, volume) plus the
//! flat list of action rows (Settings, Camera, Power, WiFi, Bluetooth). The
//! WiFi/Bluetooth rows push detail levels onto an internal `nav_stack`; the
//! stack mechanics arrive in T14. Level 1 here just exposes the primitives
//! the chrome widget will render against.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` — the
//! QuickSettings popover (brightness/volume sliders + multi-level drill-in).
//!
//! State model:
//! - `brightness`, `volume`: `f32` in `[0.0, 100.0]`. Setters clamp.
//! - `nav_stack`: starts with `vec![QsLevel::Root]`; `push_level`/`pop_level`
//!   (landing in T14) drive WiFi/BT drill-in. The field is public-crate so
//!   T14 can extend without another refactor.

/// A single frame in the QuickSettings navigation stack.
///
/// Level 1 only constructs `Root`; the `Wifi*`/`Bluetooth*` variants are
/// consumed by T14's drill-in logic but declared up-front so the enum doesn't
/// get rewritten mid-plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QsLevel {
    Root,
    Wifi,
    WifiDetails(String),
    Bluetooth,
    BluetoothDetails(String),
}

/// Logical state for the QuickSettings popover.
pub struct QuickSettings {
    brightness: f32,
    volume: f32,
    pub(crate) nav_stack: Vec<QsLevel>,
}

impl QuickSettings {
    /// Construct a popover rooted at `QsLevel::Root`, brightness and volume
    /// both initialised to 50%.
    pub fn new() -> Self {
        Self {
            brightness: 50.0,
            volume: 50.0,
            nav_stack: vec![QsLevel::Root],
        }
    }

    pub fn brightness(&self) -> f32 {
        self.brightness
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Set brightness, clamped to `[0.0, 100.0]`.
    pub fn set_brightness(&mut self, value: f32) {
        self.brightness = value.clamp(0.0, 100.0);
    }

    /// Set volume, clamped to `[0.0, 100.0]`.
    pub fn set_volume(&mut self, value: f32) {
        self.volume = value.clamp(0.0, 100.0);
    }

    /// The currently visible level.
    ///
    /// The stack is seeded with `QsLevel::Root` in `new`, so this is always
    /// safe to unwrap.
    pub fn current_level(&self) -> &QsLevel {
        self.nav_stack
            .last()
            .expect("nav_stack always has at least Root")
    }

    /// Push a new level onto the nav stack (drill into WiFi/BT details).
    ///
    /// The widget layer renders a back-chevron once `nav_stack.len() > 1`,
    /// and Plan 1's spring animation slides the new level in from the right.
    pub fn push_level(&mut self, level: QsLevel) {
        self.nav_stack.push(level);
    }

    /// Pop the top level. No-op when only `Root` remains so the popover can't
    /// be emptied.
    pub fn pop_level(&mut self) {
        if self.nav_stack.len() > 1 {
            self.nav_stack.pop();
        }
    }
}

impl Default for QuickSettings {
    fn default() -> Self {
        Self::new()
    }
}
