//! Portable display snapshots and typed chrome requests. No system probing or effects.
use tilo_desktop_model::DesktopTree;
use vello::kurbo::Rect;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Offline,
    Wired,
    Wifi,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkStatus {
    pub kind: LinkKind,
    pub name: Option<String>,
    pub signal: u8,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeState {
    pub level: f32,
    pub muted: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Microphone,
    Camera,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveMediaUse {
    pub client_id: u32,
    pub label: String,
    pub kind: MediaKind,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemStatus {
    pub battery_percent: Option<u8>,
    pub battery_charging: bool,
    pub battery_low: bool,
    pub battery_critical: bool,
    pub link: Option<LinkStatus>,
    pub volume: Option<VolumeState>,
    pub media: Vec<ActiveMediaUse>,
    pub summary: String,
}
impl SystemStatus {
    pub fn battery_percent(&self) -> Option<u8> {
        self.battery_percent
    }
    pub fn battery_is_charging(&self) -> bool {
        self.battery_charging
    }
    pub fn battery_is_low(&self) -> bool {
        self.battery_low
    }
    pub fn battery_is_critical(&self) -> bool {
        self.battery_critical
    }
    pub fn audio_is_silent(&self) -> bool {
        self.volume
            .is_some_and(|v| v.muted || v.level <= f32::EPSILON)
    }
    pub fn summary(&self) -> String {
        if self.summary.is_empty() {
            crate::i18n::t("desktop-system-status")
        } else {
            self.summary.clone()
        }
    }
}
pub fn link_bars(link: &LinkStatus) -> u8 {
    if link.kind == LinkKind::Wifi {
        link.signal.clamp(1, 4)
    } else {
        0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockSnapshot {
    pub unix_seconds: i64,
    pub clock: String,
    pub weekday: String,
    pub date: String,
}
impl Default for ClockSnapshot {
    fn default() -> Self {
        Self {
            unix_seconds: 0,
            clock: "--:--".into(),
            weekday: String::new(),
            date: String::new(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TelemetrySnapshot {
    pub cpu_percent: Option<u8>,
    pub ram_percent: Option<u8>,
    pub temperature_celsius: Option<i16>,
    pub cpu_history: Vec<u8>,
}
/// The small piece of the room's history passive chrome shows. The history
/// document stays authoritative.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityGlance {
    pub headline: Option<String>,
    pub unread: usize,
    /// Changes whenever a new latest entry is observed, even when the text is
    /// coincidentally identical, so the bar can restart its brief reveal.
    pub pulse: u64,
    pub focus: Option<ActivityFocus>,
}

/// Intent-addressable destination inside History. Device-local; links from
/// chrome, search and agents all resolve through the same shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityFocus {
    /// Exact room-owned task to select in the existing Task Center.
    pub task_id: Option<String>,
    pub author: Option<String>,
    pub group_id: Option<String>,
    pub entry_id: Option<[u8; 16]>,
    /// Stable record/resource identity. Paths are deliberately excluded:
    /// rename and move must not break a Files → Activity hand-off.
    pub resource_id: Option<String>,
    /// App-native immutable version identity, retained for detail renderers.
    pub resource_version: Option<[u8; 16]>,
}

/// A widget the top bar's centre selector can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TopBarWidget {
    Clock,
    SystemMonitor,
    Calendar,
}

impl TopBarWidget {
    pub const ALL: [Self; 3] = [Self::Clock, Self::SystemMonitor, Self::Calendar];
}

/// The person's ordered choice of centre widgets: never empty, never
/// repeating. The host persists it; the bar edits it through
/// [`ChromeIntent::SetTopBarWidgets`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopBarWidgets {
    pub widgets: Vec<TopBarWidget>,
}

impl Default for TopBarWidgets {
    fn default() -> Self {
        Self {
            widgets: TopBarWidget::ALL.to_vec(),
        }
    }
}

impl TopBarWidgets {
    #[must_use]
    pub fn normalised(mut self) -> Self {
        let mut seen = std::collections::BTreeSet::new();
        self.widgets.retain(|widget| seen.insert(*widget));
        if self.widgets.is_empty() {
            self.widgets.push(TopBarWidget::Clock);
        }
        self
    }
}
#[derive(Clone, Default)]
pub struct ChromeOverlays {
    pub overview: bool,
    pub ws_settings: bool,
    pub clock: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClockActivity {
    pub activity_id: Option<String>,
    pub label: String,
    pub remaining_secs: u64,
    pub stopwatch: bool,
    pub paused: bool,
    pub overlay: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActiveExposure {
    pub peer: u64,
    pub surface: u64,
    pub can_control: bool,
}
#[derive(Clone, Default)]
pub struct DesktopChromeSnapshot {
    pub revision: u64,
    pub status: SystemStatus,
    pub clock: ClockSnapshot,
    pub telemetry: TelemetrySnapshot,
    pub overlays: ChromeOverlays,
    pub fullscreen: bool,
    pub notification_badge: u32,
    pub activity_glance: ActivityGlance,
    pub viewport_size: Option<(f64, f64)>,
    pub clock_activity: Option<ClockActivity>,
    pub active_exposures: Vec<ActiveExposure>,
    /// The person's centre widgets; `None` until the host has read them.
    pub topbar_widgets: Option<TopBarWidgets>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromeIntent {
    ToggleOverview,
    SwitchWorkspace(i32),
    ToggleWorkspaceSettings,
    ToggleQuickSettings,
    ShowQuickSettings,
    ToggleCollaborators,
    /// Open History, at `focus` when the bar names one.
    ShowActivity(Option<ActivityFocus>),
    HideClock,
    ClockCommand {
        command: String,
        activity_id: Option<String>,
    },
    /// Persist the person's reordered or edited centre widgets.
    SetTopBarWidgets(TopBarWidgets),
    /// Focus the tab another of this person's devices offered, here.
    AcceptContinuity,
    /// Hide the continuity offer until that device's focus moves again.
    DismissContinuity,
    /// Deliver a click to a running application's tray item.
    AppTray {
        key: String,
        request: AppTrayRequest,
    },
}

/// Whether a tray item wants attention; passive items are not drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppTrayStatus {
    Passive,
    Active,
    NeedsAttention,
}

/// A tray item's picture: its own pixels, or a letter badge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppTrayIcon {
    /// Straight-alpha RGBA, row-major.
    Pixels {
        width: u32,
        height: u32,
        rgba: std::sync::Arc<Vec<u8>>,
    },
    Letter(char),
}

/// One running application's status-notifier item, as the bar draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppTrayItem {
    /// Stable key the host addresses the item by.
    pub key: String,
    /// The application's own id for the item.
    pub id: String,
    pub status: AppTrayStatus,
    pub icon: AppTrayIcon,
    /// The item has no primary action; activating it opens its menu.
    pub item_is_menu: bool,
}

/// A click on a tray item, at whole-pixel pointer coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppTrayRequest {
    Activate { x: i32, y: i32 },
    SecondaryActivate { x: i32, y: i32 },
    ContextMenu { x: i32, y: i32 },
}
pub trait DesktopChromeSource {
    fn snapshot(&self) -> DesktopChromeSnapshot;
    fn tree(&self) -> DesktopTree;
    /// Returns whether the host accepted the request, never proof of a shared mutation.
    fn intent(&self, intent: ChromeIntent) -> bool;
    /// Consume an automation request to open the primary bar's editor.
    fn take_topbar_editor_request(&self) -> bool {
        false
    }
    fn settings_anchor(&self, _rect: Rect) {}
    /// The live offer to continue a tab from another of this person's
    /// devices, as the label the bar shows ("Continue Notes from Laptop").
    fn continuity_offer(&self) -> Option<String> {
        None
    }
    /// Changes whenever the running applications' tray items change.
    fn app_tray_revision(&self) -> u64 {
        0
    }
    /// The running applications' tray items, in the host's order.
    fn app_tray(&self) -> Vec<AppTrayItem> {
        Vec::new()
    }
}
pub type Chrome = std::rc::Rc<dyn DesktopChromeSource>;
