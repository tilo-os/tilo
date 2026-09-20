//! Portable display snapshots and typed chrome requests. No system probing or effects.
use crate::WorkspaceTree;
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
            "System status".into()
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
#[derive(Clone, Default)]
pub struct ActivityGlance {
    pub headline: Option<String>,
    pub unread: usize,
    pub pulse: u64,
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
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromeIntent {
    ToggleOverview,
    SwitchWorkspace(i32),
    ToggleWorkspaceSettings,
    ToggleQuickSettings,
    ShowQuickSettings,
    ToggleCollaborators,
    ShowActivity,
    HideClock,
    ClockCommand {
        command: String,
        activity_id: Option<String>,
    },
}
pub trait DesktopChromeSource {
    fn snapshot(&self) -> DesktopChromeSnapshot;
    fn tree(&self) -> WorkspaceTree;
    /// Returns whether the host accepted the request, never proof of a shared mutation.
    fn intent(&self, intent: ChromeIntent) -> bool;
    fn settings_anchor(&self, _rect: Rect) {}
}
pub type Chrome = std::rc::Rc<dyn DesktopChromeSource>;
