//! What the shell's host reads of the machine and the people here, and what the shell's parts ask
//! of it: portable readings (the machine's status, who views a shared window, a wave, an arrival,
//! the displays' labels) and the typed requests only the host carries out. No system probing or
//! effects.
/// The machine's link, as the toolkits' surfaces draw it: the bar's status cluster and the lock
/// screen's footer share the one reading.
pub use ui::widget::{LinkKind, LinkStatus};
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
            crate::desktop::i18n::t("desktop-system-status")
        } else {
            self.summary.clone()
        }
    }
}

/// One viewer of one window this device shares: a device that asked to see it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActiveExposure {
    pub peer: u64,
    /// The tab the window stands in on this device's shell.
    pub surface: u64,
    pub can_control: bool,
    /// Whether anything of the window goes out to the devices showing it now, as its share's
    /// reading says (`streaming`). While nothing does, the viewer asked to see the window and
    /// sees nothing: the bar says it waits, and never that it watches or is in control.
    pub streaming: bool,
    /// What the workspace calls the viewer: another person by the name they go by, the person's
    /// own other device by what that device calls itself. Empty when nothing names it: the bar
    /// then says so in words, and a handle or a key is never a name.
    pub name: String,
}
/// Whether the top bar holds its strip of the screen or shows over the windows. The person chooses
/// between the first and the other two (`desktop_controls.hide_top_bar`); the host moves between
/// those two as the pointer, the keyboard and the bar's own menus ask for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TopBarPresence {
    /// Holds its strip of the screen; the windows start below it.
    #[default]
    Docked,
    /// Out of the way until reached for: the windows take its strip, and the bar neither paints nor
    /// takes the pointer.
    Hidden,
    /// Reached for: drawn over the windows' top edge, in the same place it is docked. Nothing under
    /// it moves, coming or going.
    Shown,
}

impl TopBarPresence {
    /// Whether the bar takes no room in the layout.
    #[must_use]
    pub fn overlays(self) -> bool {
        self != Self::Docked
    }
}

/// Whether the host lets the person at this desktop invite others here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Invite {
    /// Nothing here invites: no invite is offered, and nothing says why.
    #[default]
    Unavailable,
    Offered,
    /// The workspace answered that this person's part in it does not invite: where an invite
    /// would stand, that is said instead.
    Withheld,
}

impl Invite {
    /// [`Self::Offered`] when `offered` .
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

/// The person at this desktop arrived where others are (joined the workspace, or came back to it
/// with someone new there). The bar says so on the faces it already shows, never over the windows:
/// those faces pulse and a short name slides out of them, then goes on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrivalNotice {
    /// Who is here, by roster id: the faces that pulse.
    pub people: Vec<String>,
    /// Their names, in the roster's order.
    pub names: Vec<String>,
    /// The first time in a public workspace: what its people see of the person, in a few words.
    /// Pressing the status opens the person's own menu, where they choose otherwise.
    pub seen: Option<String>,
    /// When the person arrived.
    pub at: ui::time::Instant,
}

impl ArrivalNotice {
    /// What slides out of the faces: the first person's short name, and how many others ("Ada",
    /// "Ada and 2 others"), or what the people here see of the person when that is told.
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
            crate::desktop::i18n::t_args("topbar-arrived-one", &[("name", short.into())])
        } else {
            crate::desktop::i18n::t_args(
                "topbar-arrived-more",
                &[("name", short.into()), ("count", others.into())],
            )
        }
    }
}

/// What a part of the shell, or a sheet over it, asks of its host that only the host carries out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromeIntent {
    TogglePeople,
    /// Invite someone to this workspace: the host's own invite flow, in one step.
    Invite,
    /// Add a device of this person's: the ticket a new device joins them with.
    PairDevice,
    /// Open who is here at this person (their roster id): their row, with what they are doing and
    /// the wave.
    ShowPerson(String),
    /// Open History on the workspace in front.
    ShowActivity,
    /// Give the active workspace this name.
    RenameWorkspace {
        name: String,
    },
    /// Take the active workspace off this device's desktop, leaving it to the people in it.
    LeaveWorkspace,
    /// Delete the active workspace, with everything in it, now: the person already answered a
    /// question of their own about it.
    DeleteWorkspaceConfirmed,
}

/// One display's card: the number Settings shows it by, and the name Settings calls it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    /// The display, by its monitor as the desktop names it.
    pub monitor: String,
    /// Its place in the person's layout, counting from one.
    pub number: usize,
    /// What Settings calls it: the built-in display, or the monitor's model.
    pub name: String,
}

/// What Settings asked for: which request it is (asking again shows the cards again) and each
/// display's card.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ask {
    /// The request's stamp: the moment it was written, which changes with every press of Identify.
    pub id: u64,
    /// One per display the session shows something on.
    pub labels: Vec<Label>,
}
