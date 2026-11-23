//! Window warmth: what the shell tells every app window about the workspace
//! it stands in.
//!
//! A window takes the temperature of the workspace it is shown in, the
//! colour of the person at this machine, the shell's theme, and where its
//! workspace stands with its peers. The shell works that out
//! ([`tilo_session_core::desktop::warmth::WindowWarmth`]), this follows the
//! settings it comes from, and [`AppTabs::publish_window_warmth`] sends it
//! to each open window when it differs from what that window was told.
//!
//! The temperatures are looked at again every few seconds even while the
//! person stays in one workspace, so a wallpaper that arrives late or is
//! changed retints its workspace. The settings are read when they change,
//! never per frame.

use std::time::Instant;

use serde_json::{Map, Value};
use tilo_desktop_model::Layout;
use tilo_desktop_surface::DesktopHandle;
use tilo_session_core::desktop::warmth::WindowWarmth;
use ui::Theme;
use ui::floating_surface::HostNetworkStatus;
use ui::tokens::PresenceHue;
use ui::workspace_sync::WorkspaceSync;

use crate::app_tabs::AppTabs;
use crate::wallpapers::Wallpapers;

/// What every window is told of the shell it stands on, and the settings that decide it as last
/// read.
pub(crate) struct Windows {
    warmth: WindowWarmth,
    /// The theme windows are told: the shell's theme as the settings last made it.
    theme: Theme,
    /// The person's settings as last read: the workspace temperatures and their own colour come
    /// from it.
    document: Map<String, Value>,
    /// What version of the settings `document` is, so it is read again only when it changed.
    settings: settings_store::SettingsFollower,
}

impl Windows {
    /// The windows of a shell wearing `theme` , with `person` sitting at this machine.
    pub(crate) fn new(theme: Theme, person: &str) -> Self {
        // Record the stamp before reading the document so a write that lands in between moves the
        // stamp ahead of what was recorded and is read on the next pass rather than missed.
        let mut settings = settings_store::SettingsFollower::default();
        settings.changed(settings_store::stamp());
        let document = settings_store::load();
        let your_colour = PresenceHue::worn_in(&Value::Object(document.clone()), person);
        Self {
            warmth: WindowWarmth::new(your_colour),
            theme: theme.with_your_colour(your_colour),
            document,
            settings,
        }
    }

    /// The theme the shell and its windows wear, in the person's own colour.
    pub(crate) fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Read the person's settings again when they changed, with `person` sitting at this machine:
    /// the shell's theme as the changed settings make it (dark mode in the quick settings, a theme
    /// chosen in Settings), for everything the session draws to wear. Nothing reads the document on
    /// a pass where it did not change.
    pub(crate) fn follow_settings(&mut self, person: &str) -> Option<Theme> {
        if !self.settings.changed(settings_store::stamp()) {
            return None;
        }
        self.document = settings_store::load();
        let your_colour = PresenceHue::worn_in(&Value::Object(self.document.clone()), person);
        self.warmth.set_your_colour(your_colour);
        self.theme = shell_theme(&self.document).with_your_colour(your_colour);
        Some(self.theme.clone())
    }

    /// Give every open window its appearance, and look at the temperatures
    /// when they are due (a workspace switch, a new wallpaper, or the few
    /// seconds between looks), with
    /// `wallpapers` holding the pictures each workspace shows, and `network`
    /// what this device reads of the network.
    pub(crate) fn follow(
        &mut self,
        tabs: &mut AppTabs,
        handle: &DesktopHandle,
        wallpapers: &Wallpapers,
        network: HostNetworkStatus,
        now: Instant,
    ) {
        handle.read(|tree| {
            let active = Some(tree.active);
            if self.warmth.due(active, now) {
                let settings = Value::Object(self.document.clone());
                let workspaces: Vec<_> = tree
                    .workspaces
                    .iter()
                    .map(tilo_desktop_model::Workspace::id)
                    .collect();
                self.warmth.update(&workspaces, active, now, &mut |id| {
                    let id = id.to_string();
                    ui::theme::workspace_temperature_override(&settings, &id)
                        .or_else(|| wallpapers.hue(&id))
                });
            }
            tabs.publish_window_warmth(
                |app| self.warmth.appearance_of(tree, app, &self.theme),
                |app, instance| workspace_sync(tree, app, instance),
                &self.theme,
                network,
            );
        });
    }
}

/// Where the workspace the window `instance` names stands with its peers.
///
/// The window's workspace answers, never another window's: an app may have a window in each
/// of two workspaces, and being told the other's state shows a record as kept where it is not.
/// `None` falls back to `app`, for a connection that has not named its window yet.
fn workspace_sync(tree: &Layout, app: &str, instance: Option<u64>) -> Option<WorkspaceSync> {
    let workspace = window_workspace(tree, app, instance)?;
    tilo_network_actor::workspace_sync::told(workspace.id(), &workspace.name)
}

/// The workspace the window `instance` of `app` is in: the window's own, never another window's of
/// the same app; `None` falls back to `app`, for a connection that has not named its window yet.
pub(crate) fn window_workspace<'a>(
    tree: &'a Layout,
    app: &str,
    instance: Option<u64>,
) -> Option<&'a tilo_desktop_model::Workspace> {
    instance
        .and_then(|instance| {
            tree.workspaces.iter().find(|workspace| {
                workspace.tabs().any(|shown| {
                    shown.id == instance
                        && tilo_desktop_model::apps::canonical_app_id(&shown.tile)
                            .is_some_and(|id| tilo_desktop_model::apps::app_ids_match(&id, app))
                })
            })
        })
        .or_else(|| tree.workspace_holding_app(app))
}

/// The theme the shell and its windows wear: the settings document's, with the host's accessibility
/// overrides on top, built as the shell's own theme was when the session started.
fn shell_theme(document: &Map<String, Value>) -> Theme {
    let accessibility = ui::process::environment().accessibility;
    Theme::configured_from(document).with_a11y(
        &ui::a11y::A11ySettings {
            high_contrast: accessibility.high_contrast,
            reduce_motion: accessibility.reduce_motion,
            ..Default::default()
        },
        accessibility.text_scale.unwrap_or(1.0),
        accessibility.ui_scale.unwrap_or(1.0),
    )
}

/// What this device reads of the network, as an app window may be told it: whether it is on one,
/// and nothing of which. `None` until a service read the connection; it does not read whether the
/// link is metered.
pub(crate) fn network_status(readings: &crate::status::Shared) -> HostNetworkStatus {
    let link = readings.borrow().link();
    let Some(link) = link else {
        return HostNetworkStatus::default();
    };
    HostNetworkStatus {
        available: Some(link.kind != tilo_desktop::chrome::LinkKind::Offline),
        metered: None,
    }
}

impl crate::Session {
    /// Tell every open app window the temperature of the workspace it is in and the colour of the
    /// person at this machine, whenever that differs from what that window was told: when it opens,
    /// when it moves to another workspace, when that workspace's picture or temperature changes,
    /// and when the person chooses another colour.
    pub(crate) fn follow_window_warmth(&mut self) {
        // A theme the person's settings changed to is what the shell, the chrome above the windows
        // (the search palette among it) and the shell draw in from this frame, as every window is
        // told it below.
        if let Some(theme) = self.warmth.follow_settings(&self.shared.person) {
            self.shared.desktop.set_theme(theme.clone());
            self.shared.chrome.borrow_mut().set_theme(theme.clone());
            self.shared.people.borrow().set_theme(theme.clone());
            self.readout.borrow_mut().set_theme(theme.clone());
            if let Some(banners) = &self.banner_surface {
                banners.borrow_mut().set_theme(theme.clone());
            }
            crate::sheets::set_theme(&theme);
            if let Some(shell) = self.shell.as_mut() {
                shell.set_theme(&theme);
            }
        }
        let Some(tabs) = &mut self.tabs else {
            return;
        };
        let network = network_status(&self.shared.readings);
        self.warmth.follow(
            tabs,
            &self.shared.handle,
            &self.shared.wallpapers,
            network,
            ui::time::now(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::ids::{GroupId, StripId, TabId, WorkspaceId};
    use tilo_desktop_model::{Strip, Tab, TileContent, WindowGroup, WindowWidth, Workspace};

    const APP: &str = ui::apps::TASKS;

    /// A workspace named `name` holding one window of [`APP`].
    fn workspace(id: u64, name: &str, tab: TabId) -> Workspace {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(u128::from(id)), name);
        let mut strip = Strip::new(StripId::from(id), name);
        strip.window_groups.push(WindowGroup {
            id: GroupId::from(id),
            tabs: vec![Tab {
                id: tab,
                title: "Tasks".into(),
                tile: TileContent::LocalApp {
                    app_id: APP.into(),
                    label: "Tasks".into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.strips.push(strip);
        workspace
    }

    /// Two windows of one app, each in a workspace of its own: each hears where that workspace
    /// stands, never the other's.
    #[test]
    fn a_window_is_told_its_workspace_never_another_windows() {
        let tree = Layout {
            workspaces: vec![workspace(2, "Shared", 200), workspace(1, "Mine", 100)],
            ..Layout::default()
        };
        let of = |instance| window_workspace(&tree, APP, Some(instance)).map(|w| w.name.clone());
        assert_eq!(of(100).as_deref(), Some("Mine"));
        assert_eq!(of(200).as_deref(), Some("Shared"));
        assert_eq!(
            of(999).as_deref(),
            Some("Shared"),
            "a connection that named no window falls back to the first workspace holding the app"
        );
    }

    /// A settings write that lands between `Windows::new` taking its stamp and loading the
    /// document leaves the stamp ahead of what was recorded, so the next `follow_settings` pass
    /// reads the update instead of missing it.
    #[test]
    fn a_change_during_construction_is_followed_on_the_next_pass() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct RacingSource {
            inner: settings_store::HostDocument,
            race: AtomicBool,
        }

        impl settings_store::SettingsSource for RacingSource {
            fn load(&self) -> Map<String, Value> {
                let document = self.inner.load();
                if self.race.swap(false, Ordering::AcqRel) {
                    let dark = serde_json::json!({"mode": "dark"})
                        .as_object()
                        .cloned()
                        .unwrap_or_default();
                    self.inner.update_section("appearance", dark).unwrap();
                }
                document
            }

            fn update_section(
                &self,
                name: &str,
                values: Map<String, Value>,
            ) -> std::io::Result<()> {
                self.inner.update_section(name, values)
            }

            fn revision(&self) -> u64 {
                self.inner.revision()
            }

            fn changes(&self) -> &settings_store::ChangeSignal {
                self.inner.changes()
            }
        }

        let initial = serde_json::json!({"appearance": {"mode": "light"}})
            .as_object()
            .cloned()
            .unwrap_or_default();
        let source: Arc<dyn settings_store::SettingsSource> = Arc::new(RacingSource {
            inner: settings_store::HostDocument::new(initial, |_| Ok(())),
            race: AtomicBool::new(true),
        });
        let _scope = settings_store::scoped_source(Some(source));

        let mut windows = Windows::new(Theme::light(), PERSON);
        assert_eq!(
            windows.document["appearance"]["mode"],
            serde_json::json!("light")
        );
        assert!(
            windows.follow_settings(PERSON).is_some(),
            "the write that landed during construction is read on the next pass"
        );
        assert_eq!(
            windows.document["appearance"]["mode"],
            serde_json::json!("dark")
        );
        assert!(
            windows.follow_settings(PERSON).is_none(),
            "once caught up, an unchanged stamp reads nothing again"
        );
    }

    /// With no settings store (`stamp()` is `None`), `Windows::new` loads once and
    /// `follow_settings` skips later passes.
    #[test]
    fn without_a_settings_store_windows_follow_skips_reload() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", "").remove("XDG_DATA_HOME");
        let _scope = settings_store::scoped_source(None);
        assert_eq!(settings_store::stamp(), None);

        let mut windows = Windows::new(Theme::light(), PERSON);
        assert!(
            windows.follow_settings(PERSON).is_none(),
            "a follow pass with still no stamp does not reload"
        );
    }
}
