//! The window topology: strips of window groups of tabs.

use serde::{Deserialize, Serialize};

use crate::app::AppKind;
use crate::ids::{GroupId, StripId, TabId};
use crate::width::WindowWidth;

/// What a tile holds. Persistence only carries `Stub` and a symbolic
/// `AppPlaceholder` for native apps, whose concrete widgets are
/// constructed at shell boot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TileContent {
    /// Device-local workspace history. It is a normal tiled window for this
    /// compositor, but it is never projected into the room document: the
    /// departure frontier and what this person has seen are private state.
    WorkspaceActivity,
    AppPlaceholder {
        kind: AppKind,
        label: String,
    },
    Stub {
        kind: AppKind,
        label: String,
    },
    /// Foreign Wayland client adopted by the compositor. `surface_id` is
    /// the `wl_surface` protocol id the compositor maps back to a smithay
    /// `WlSurface`; renders as a Stub until the frame loop blits it.
    Foreign {
        app_id: String,
        surface_id: u64,
    },
    /// Durable local application window awaiting a fresh compositor surface.
    /// Unlike `Foreign`, this stores no process-local Wayland id. At session
    /// startup the normal launcher opens `app_id` and binds its first
    /// toplevel back into this exact tab, preserving layout without reviving
    /// a dead surface handle.
    LocalApp {
        app_id: String,
        label: String,
        #[serde(default)]
        note: String,
    },
    /// A window a peer has open, before this machine has bound a local
    /// instance to the tab. The peer's `surface_id` only resolves on
    /// their compositor so it is dropped, keeping only the app id. A
    /// waypoint: `shell::peer_windows` binds a local instance and the
    /// tile becomes `Foreign`. Never projected into a room document,
    /// since a `PeerWindow` is by construction somebody else's.
    PeerWindow {
        /// The wayland app-id the peer's window announces.
        app_id: String,
        /// The peer's own resolved tab title.
        label: String,
        /// What this machine is doing about it, for the card. Empty means
        /// nobody has looked yet; writing the note is how
        /// `shell::peer_windows` records that the decision was made, so a
        /// refusal or launch only ever starts once.
        #[serde(default)]
        note: String,
        /// Set when this machine has no copy of the app to open: whether
        /// one can be installed here, or why it cannot. The card offers the
        /// install from this, never from the wording of `note`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        missing: Option<MissingApp>,
    },
    /// A legacy (X11 / Wayland) window that runs only on the device `host`
    /// (its hex device key), shown on every other device as that window,
    /// never relaunched there: a legacy app's state lives in its process,
    /// not in a record another device could open. The tab id and `host`
    /// stay stable for the window's life, so a per-window stream from the
    /// host can attach to this tab. On the hosting device the same tab is
    /// its live [`Self::Foreign`] surface.
    HostedWindow {
        /// The wayland app-id or X11 class the window announces.
        app_id: String,
        /// The window's title on the hosting device.
        label: String,
        /// Hex device key (iroh endpoint id) of the hosting device.
        host: String,
        /// The hosting device's own name, as its certificate labels it.
        #[serde(default)]
        host_label: String,
    },
}

/// Why a window another device has open has no instance here yet.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MissingApp {
    /// Not installed, and a configured registry offers it to this device:
    /// the person may install it, which opens it in the same tab.
    Installable {
        /// The registry's name for the app, for the install button.
        display_name: String,
    },
    /// Not installed, and nothing this device can install provides it.
    Unavailable,
}

impl TileContent {
    /// Shell-owned personal surfaces participate in local window management
    /// without becoming windows on collaborators' desktops.
    pub fn is_device_local(&self) -> bool {
        matches!(self, Self::WorkspaceActivity)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub tile: TileContent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowGroup {
    pub id: GroupId,
    pub tabs: Vec<Tab>,
    pub active_tab: TabId,
    pub width: WindowWidth,
    /// Mirrors the widget's maximized flag. This is presentation state for
    /// this device: it survives a local restart but is never sent to room
    /// peers. `#[serde(default)]` keeps pre-existing local JSON loading
    /// un-maximized.
    #[serde(default)]
    pub maximized: bool,
    /// The tabs that were in front before the current one, most recent last.
    /// Like `active_tab` itself this is what this device is showing: never
    /// replicated or persisted. Closing the front tab goes back to the most
    /// recent of these that is still open. Written only by
    /// [`WindowGroup::activate_tab`].
    #[serde(skip)]
    pub focus_history: Vec<TabId>,
}

impl WindowGroup {
    /// Bring `tab` to the front, remembering the tab it replaces. The one
    /// place a *selection* is recorded, whoever makes it (a click, a
    /// keyboard shortcut, a protocol activation, a new window opening).
    /// Does nothing if `tab` is already in front.
    pub fn activate_tab(&mut self, tab: TabId) {
        if self.active_tab == tab {
            return;
        }
        let previous = self.active_tab;
        if self.tabs.iter().any(|t| t.id == previous) {
            self.focus_history.retain(|t| *t != previous);
            self.focus_history.push(previous);
        }
        self.focus_history.retain(|t| *t != tab);
        self.active_tab = tab;
        // Only tabs still in the group can be returned to.
        let tabs = &self.tabs;
        self.focus_history
            .retain(|t| tabs.iter().any(|x| x.id == *t));
    }

    /// The tab to show after `closed` goes away: the most recently shown one
    /// that is still open, if any.
    pub fn previously_active(&self, closed: TabId) -> Option<TabId> {
        self.focus_history
            .iter()
            .rev()
            .copied()
            .find(|t| *t != closed && self.tabs.iter().any(|x| x.id == *t))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Strip {
    pub id: StripId,
    pub name: String,
    pub window_groups: Vec<WindowGroup>,
}

/// What a strip is for. Read from its id ([`Strip::kind`]), never stored
/// beside it: every member derives the same Lounge id from the room, so a
/// stored kind could only ever disagree with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripKind {
    /// A strip a person or an agent made: its device's to name and remove.
    Regular,
    /// The workspace's shared place (chat, tasks and notes): in every
    /// workspace, first, owned by nobody, never renamed or removed.
    Lounge,
}

impl Strip {
    /// Whether this is a regular strip or the workspace's Lounge.
    pub fn kind(&self) -> StripKind {
        if crate::ids::is_shared(self.id.raw()) {
            StripKind::Lounge
        } else {
            StripKind::Regular
        }
    }

    pub fn new(id: StripId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            window_groups: Vec::new(),
        }
    }
}
