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
    pub author: Option<String>,
    pub group_id: Option<String>,
    pub entry_id: Option<[u8; 16]>,
    /// Stable record/resource identity. Paths are deliberately excluded:
    /// rename and move must not break a Files → Activity hand-off.
    pub resource_id: Option<String>,
    /// App-native immutable version identity, retained for detail renderers.
    pub resource_version: Option<[u8; 16]>,
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
/// Whether the top bar holds its strip of the screen or shows over the
/// windows. The person chooses between the first and the other two
/// (`desktop_controls.hide_top_bar`); the host moves between those two as
/// the pointer, the keyboard and the bar's own menus ask for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TopBarPresence {
    /// Holds its strip of the screen; the windows start below it.
    #[default]
    Docked,
    /// Out of the way until reached for: the windows take its strip, and
    /// the bar neither paints nor takes the pointer.
    Hidden,
    /// Reached for: drawn over the windows' top edge, in the same place it
    /// is docked. Nothing under it moves, coming or going.
    Shown,
}

impl TopBarPresence {
    /// Whether the bar takes no room in the layout.
    #[must_use]
    pub fn overlays(self) -> bool {
        self != Self::Docked
    }
}

#[derive(Clone, Default)]
pub struct DesktopChromeSnapshot {
    pub revision: u64,
    pub status: SystemStatus,
    pub clock: ClockSnapshot,
    pub overlays: ChromeOverlays,
    pub fullscreen: bool,
    pub notification_badge: u32,
    /// Fresh banners are being held (Do Not Disturb, presenting, a focus
    /// schedule): the bar's status cluster shows a silenced bell.
    pub notifications_quiet: bool,
    /// The host has no quick settings: the status cluster is the bell that
    /// opens the notification centre, named and drawn as one, not a
    /// settings glyph promising settings that are not there.
    pub notifications_only: bool,
    pub activity_glance: ActivityGlance,
    pub viewport_size: Option<(f64, f64)>,
    pub clock_activity: Option<ClockActivity>,
    pub active_exposures: Vec<ActiveExposure>,
    /// People following the room without taking part (spectators). The bar
    /// counts them beside the avatars so an audience is never invisible.
    pub watching: u32,
    /// Docked, or hidden and shown over the windows.
    pub top_bar: TopBarPresence,
    /// The person at this desktop as everyone here sees them, for the
    /// bar's own avatar; `None` paints none.
    pub own: Option<OwnChip>,
    /// Whether the person may invite others here: the bar keeps its invite
    /// button beside the faces, however many people are present.
    pub invite: Invite,
    /// The last wave at this person: the bar says who waved, for a moment
    /// after it came.
    pub waved: Option<WaveNotice>,
    /// The person arrived where others are: their faces pulse in the bar
    /// and a short name slides out of them, for a moment.
    pub arrived: Option<ArrivalNotice>,
}

/// Whether the host lets the person at this desktop invite others here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Invite {
    #[default]
    Unavailable,
    Offered,
}

impl Invite {
    /// [`Self::Offered`] when `offered`.
    #[must_use]
    pub fn when(offered: bool) -> Self {
        if offered {
            Self::Offered
        } else {
            Self::Unavailable
        }
    }
}

/// Someone waved at the person at this desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaveNotice {
    /// Who waved, by their name here.
    pub from: String,
    /// When the wave came.
    pub at: ui::time::Instant,
}

/// The person at this desktop arrived where others are (joined the
/// workspace, or came back to it with someone new there). The bar says so
/// on the faces it already shows, never over the windows: those faces pulse
/// and a short name slides out of them, then goes on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrivalNotice {
    /// Who is here, by roster id: the faces that pulse.
    pub people: Vec<String>,
    /// Their names, in the roster's order.
    pub names: Vec<String>,
    /// The first time in a public workspace: what its people see of the
    /// person, in a few words. Pressing the status opens the person's own
    /// menu, where they choose otherwise.
    pub seen: Option<String>,
    /// When the person arrived.
    pub at: ui::time::Instant,
}

impl ArrivalNotice {
    /// What slides out of the faces: the first person's short name, and
    /// how many others ("Ada", "Ada and 2 others"), or what the people
    /// here see of the person when that is told.
    #[must_use]
    pub fn text(&self) -> String {
        if let Some(seen) = &self.seen {
            return seen.clone();
        }
        let Some(first) = self.names.first() else {
            return String::new();
        };
        // A short name: the first word of the name everyone here sees.
        let short = first.split_whitespace().next().unwrap_or(first);
        let others = self.names.len().saturating_sub(1);
        if others == 0 {
            crate::i18n::t_args("topbar-arrived-one", &[("name", short.into())])
        } else {
            crate::i18n::t_args(
                "topbar-arrived-more",
                &[("name", short.into()), ("count", others.into())],
            )
        }
    }
}

/// The person at this desktop, as their own avatar in the bar shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnChip {
    /// Their name as everyone here sees it, numbered when shared.
    pub name: String,
    /// Their colour.
    pub rgb: (u8, u8, u8),
    /// Who they are (their root key, hex), for the picture they chose;
    /// empty where the host names none.
    pub identity: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromeIntent {
    ToggleOverview,
    SwitchWorkspace(i32),
    ToggleQuickSettings,
    ShowQuickSettings,
    ToggleCollaborators,
    /// Invite someone to this workspace, straight from the bar's invite
    /// button: the host's own invite flow, in one step.
    Invite,
    /// Open who is here at this person (their roster id): their row, with
    /// what they are doing and the wave.
    ShowPerson(String),
    /// Open History, at `focus` when the bar names one.
    ShowActivity(Option<ActivityFocus>),
    HideClock,
    ClockCommand {
        command: String,
        activity_id: Option<String>,
    },
    /// Focus the tab another of this person's devices offered, here.
    AcceptContinuity,
    /// Hide the continuity offer until that device's focus moves again.
    DismissContinuity,
    /// Show the ask waiting as a dot on the person's own avatar
    /// ([`DesktopChromeSource::avatar_ask`]).
    OpenAvatarAsk,
    /// Fold the open ask into the avatar's dot: a click elsewhere, Escape.
    CollapseAvatarAsk,
    AnswerAvatarAsk,
    /// The person put the ask off ("Not now"): it goes.
    PutOffAvatarAsk,
    /// Show the active workspace's strips to choose from, in the strip
    /// switcher: a click on the strip's name after the workspace's.
    ShowStrips,
    /// Give the active workspace this name.
    RenameWorkspace {
        name: String,
    },
    /// Open or close the menu behind the bar's Tilo mark
    /// ([`crate::topbar::TopBarChrome::with_tilo_mark`]), under
    /// [`crate::topbar::TopBarChrome::tilo_mark_rect`].
    ToggleTiloMenu,
    /// Open or close the menu behind the workspace name, under
    /// [`crate::topbar::TopBarChrome::workspace_menu_rect`].
    ToggleWorkspaceMenu,
    /// Open or close the card behind the person's own avatar, under
    /// [`crate::topbar::TopBarChrome::own_avatar_rect`].
    ToggleProfile,
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
/// An ask about the person themselves (keeping who they are here), put to
/// them from their own avatar in the top bar. Open, it is a small popover
/// in the bar's band beside the avatar, so it never covers a window or
/// takes a click meant for one; clicked away it waits as a dot on the
/// avatar, which opens it again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AvatarAsk {
    /// The question ("Keep your name and work?").
    pub question: String,
    /// What answering brings, said where there is room and to assistive
    /// technology.
    pub detail: String,
    /// The answer's label ("Keep with passkey").
    pub answer: String,
    /// Shown as the popover; `false` while it waits as the avatar's dot.
    pub open: bool,
}

pub trait DesktopChromeSource {
    fn snapshot(&self) -> DesktopChromeSnapshot;
    fn tree(&self) -> DesktopTree;
    /// Returns whether the host accepted the request, never proof of a shared mutation.
    fn intent(&self, intent: ChromeIntent) -> bool;
    fn settings_anchor(&self, _rect: Rect) {}
    /// The live offer to continue a tab from another of this person's
    /// devices, as the label the bar shows ("Continue Notes from Laptop").
    fn continuity_offer(&self) -> Option<String> {
        None
    }
    /// The ask waiting on the person's own avatar, open or as its dot.
    fn avatar_ask(&self) -> Option<AvatarAsk> {
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
