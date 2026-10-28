//! The window topology: strips of window groups of tabs.

use serde::{Deserialize, Serialize};

use crate::app::AppKind;
use crate::ids::{GroupId, StripId, TabId};
use crate::width::WindowWidth;

/// What a tile holds. Persistence only carries `Stub` and a symbolic
/// `AppPlaceholder` for native apps, whose concrete widgets are
/// constructed at shell boot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(from = "TileRecord", into = "TileRecord")]
pub enum TileContent {
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

/// How a [`TileContent`] is written: every variant it was ever written
/// with, in its place, so bytes already held keep their meaning.
#[derive(Clone, Serialize, Deserialize)]
enum TileRecord {
    /// The Activity window the shell once drew itself; it reads as a
    /// window of the Activity app.
    #[serde(rename = "WorkspaceActivity")]
    RetiredWorkspaceActivity,
    AppPlaceholder {
        kind: AppKind,
        label: String,
    },
    Stub {
        kind: AppKind,
        label: String,
    },
    Foreign {
        app_id: String,
        surface_id: u64,
    },
    LocalApp {
        app_id: String,
        label: String,
        #[serde(default)]
        note: String,
    },
    PeerWindow {
        app_id: String,
        label: String,
        #[serde(default)]
        note: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        missing: Option<MissingApp>,
    },
    HostedWindow {
        app_id: String,
        label: String,
        host: String,
        #[serde(default)]
        host_label: String,
    },
}

impl From<TileRecord> for TileContent {
    fn from(record: TileRecord) -> Self {
        match record {
            TileRecord::RetiredWorkspaceActivity => Self::LocalApp {
                app_id: tilo_actions::remote::workspace_activity::APP_ID.to_owned(),
                label: "Activity".to_owned(),
                note: String::new(),
            },
            TileRecord::AppPlaceholder { kind, label } => Self::AppPlaceholder { kind, label },
            TileRecord::Stub { kind, label } => Self::Stub { kind, label },
            TileRecord::Foreign { app_id, surface_id } => Self::Foreign { app_id, surface_id },
            TileRecord::LocalApp {
                app_id,
                label,
                note,
            } => Self::LocalApp {
                app_id,
                label,
                note,
            },
            TileRecord::PeerWindow {
                app_id,
                label,
                note,
                missing,
            } => Self::PeerWindow {
                app_id,
                label,
                note,
                missing,
            },
            TileRecord::HostedWindow {
                app_id,
                label,
                host,
                host_label,
            } => Self::HostedWindow {
                app_id,
                label,
                host,
                host_label,
            },
        }
    }
}

impl From<TileContent> for TileRecord {
    fn from(tile: TileContent) -> Self {
        match tile {
            TileContent::AppPlaceholder { kind, label } => Self::AppPlaceholder { kind, label },
            TileContent::Stub { kind, label } => Self::Stub { kind, label },
            TileContent::Foreign { app_id, surface_id } => Self::Foreign { app_id, surface_id },
            TileContent::LocalApp {
                app_id,
                label,
                note,
            } => Self::LocalApp {
                app_id,
                label,
                note,
            },
            TileContent::PeerWindow {
                app_id,
                label,
                note,
                missing,
            } => Self::PeerWindow {
                app_id,
                label,
                note,
                missing,
            },
            TileContent::HostedWindow {
                app_id,
                label,
                host,
                host_label,
            } => Self::HostedWindow {
                app_id,
                label,
                host,
                host_label,
            },
        }
    }
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    /// What the tab shows: the name a person gave it, else its window's
    /// own title.
    pub title: String,
    pub tile: TileContent,
    /// The window's own latest title while a person's name stands in
    /// `title`; `None` when the tab has no such name. Written only by
    /// [`Tab::set_window_title`] and [`Tab::rename`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_title: Option<String>,
}

impl Tab {
    /// The window announced `title`. While a person's name stands, it is
    /// kept as the window's title for when the name is cleared, and the tab
    /// goes on showing the name. Returns whether anything changed: a window
    /// sending the same title again (terminals do, continuously) is no
    /// change.
    pub fn set_window_title(&mut self, title: &str) -> bool {
        let shown = self.app_title.as_mut().unwrap_or(&mut self.title);
        if shown == title {
            return false;
        }
        title.clone_into(shown);
        true
    }

    /// A person names the tab `name`. A blank name clears their name, and
    /// the tab shows its window's latest title again. Returns whether
    /// anything changed.
    pub fn rename(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return match self.app_title.take() {
                Some(window_title) => {
                    self.title = window_title;
                    true
                }
                None => false,
            };
        }
        match &self.app_title {
            Some(_) if self.title == name => false,
            Some(_) => {
                name.clone_into(&mut self.title);
                true
            }
            None => {
                self.app_title = Some(std::mem::replace(&mut self.title, name.to_owned()));
                true
            }
        }
    }

    /// Whether a person's name stands in place of the window's own title.
    #[must_use]
    pub fn is_named(&self) -> bool {
        self.app_title.is_some()
    }
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

impl Strip {
    pub fn new(id: StripId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            window_groups: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tab_name_tests {
    use super::*;

    fn terminal(title: &str) -> Tab {
        Tab {
            id: 7,
            title: title.into(),
            tile: TileContent::Foreign {
                app_id: "Alacritty".into(),
                surface_id: 3,
            },
            app_title: None,
        }
    }

    #[test]
    fn a_name_replaces_the_shown_title_and_keeps_the_windows_own() {
        let mut tab = terminal("~/src");
        assert!(tab.rename("  Build  "));
        assert_eq!(tab.title, "Build");
        assert_eq!(tab.app_title.as_deref(), Some("~/src"));
        assert!(tab.is_named());
        assert!(tab.rename("Deploy"));
        assert_eq!(tab.title, "Deploy");
        assert_eq!(tab.app_title.as_deref(), Some("~/src"));
        assert!(!tab.rename("Deploy"), "the same name again is no change");
    }

    #[test]
    fn a_window_title_while_named_updates_only_the_windows_own() {
        let mut tab = terminal("~/src");
        tab.rename("Build");
        assert!(tab.set_window_title("cargo build"));
        assert_eq!(tab.title, "Build");
        assert_eq!(tab.app_title.as_deref(), Some("cargo build"));
    }

    #[test]
    fn clearing_the_name_shows_the_latest_window_title() {
        let mut tab = terminal("~/src");
        tab.rename("Build");
        tab.set_window_title("cargo test");
        assert!(tab.rename("   "));
        assert_eq!(tab.title, "cargo test");
        assert_eq!(tab.app_title, None);
        assert!(!tab.is_named());
        assert!(!tab.rename(""), "a tab with no name has none to clear");
        assert!(tab.set_window_title("~"));
        assert_eq!(tab.title, "~");
    }

    #[test]
    fn the_same_window_title_again_is_no_change() {
        let mut tab = terminal("~/src");
        assert!(!tab.set_window_title("~/src"));
        tab.rename("Build");
        tab.set_window_title("vim");
        assert!(!tab.set_window_title("vim"));
        assert_eq!(tab.title, "Build");
    }

    #[test]
    fn a_tab_without_a_name_is_stored_as_before() {
        let json = serde_json::to_value(terminal("~/src")).expect("serialises");
        assert!(json.get("app_title").is_none());
        let old: Tab = serde_json::from_value(serde_json::json!({
            "id": 7,
            "title": "~/src",
            "tile": { "Foreign": { "app_id": "Alacritty", "surface_id": 3 } },
        }))
        .expect("a tab stored before names reads back");
        assert_eq!(old.app_title, None);
        let mut named = terminal("~/src");
        named.rename("Build");
        let back: Tab = serde_json::from_value(serde_json::to_value(&named).expect("serialises"))
            .expect("reads back");
        assert_eq!(back.title, "Build");
        assert_eq!(back.app_title.as_deref(), Some("~/src"));
    }
}

#[cfg(test)]
mod tile_record_tests {
    use super::*;

    #[test]
    fn a_tile_written_as_the_shells_activity_window_reads_as_the_activity_apps() {
        let read: TileContent = serde_json::from_str("\"WorkspaceActivity\"").unwrap();
        assert_eq!(
            read,
            TileContent::LocalApp {
                app_id: tilo_actions::remote::workspace_activity::APP_ID.into(),
                label: "Activity".into(),
                note: String::new(),
            }
        );
    }

    #[test]
    fn every_tile_reads_back_as_written() {
        let tiles = [
            TileContent::Stub {
                kind: AppKind::Terminal,
                label: "Shell".into(),
            },
            TileContent::Foreign {
                app_id: "Alacritty".into(),
                surface_id: 3,
            },
            TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            TileContent::PeerWindow {
                app_id: "so.tilo.files".into(),
                label: "Files".into(),
                note: "opening".into(),
                missing: Some(MissingApp::Unavailable),
            },
            TileContent::HostedWindow {
                app_id: "xterm".into(),
                label: "xterm".into(),
                host: "ab12".into(),
                host_label: "Desk".into(),
            },
        ];
        for tile in tiles {
            let written = serde_json::to_string(&tile).unwrap();
            assert_eq!(
                serde_json::from_str::<TileContent>(&written).unwrap(),
                tile,
                "{written}"
            );
        }
    }
}
