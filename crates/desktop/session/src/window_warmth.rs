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

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use tilo_desktop_model::Layout;
use tilo_desktop_model::ids::WorkspaceId;
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_session_core::desktop::warmth::WindowWarmth;
use tilo_ui_base::Theme;
use tilo_ui_base::tokens::PresenceHue;
use tilo_ui_base::workspace_sync::{WorkspaceSync, WorkspaceSyncState};
use ui::floating_surface::HostNetworkStatus;

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
    /// The directory the applied theme's bundle is staged in, as a window's sandbox sees it: the
    /// sets its process draws with. `None` for the theme the mode names, or a bundle this device
    /// does not have.
    bundle: Option<String>,
    /// The package `bundle` was staged for, so the staged files follow the appearance and are not
    /// rewritten on every pass.
    bundled: Option<String>,
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
        let mut windows = Self {
            warmth: WindowWarmth::new(your_colour),
            theme: theme.with_your_colour(your_colour),
            document,
            settings,
            bundle: None,
            bundled: None,
        };
        windows.stage_applied_bundle();
        windows
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
        // The staged bundle follows the appearance: a package the person installs and applies while
        // their apps run is in the directory the running windows read, without a relaunch.
        self.stage_applied_bundle();
        Some(self.theme.clone())
    }

    /// Stage the applied theme's bundle below this device's data home, where the launcher mounts it
    /// read-only for every window's process, unless it is already staged for that package. Nothing
    /// of the package store reaches a sandbox: only the bundle's document and the modules of the
    /// sets it names.
    fn stage_applied_bundle(&mut self) {
        let package = self
            .document
            .get(tilo_ui_base::theme::APPEARANCE_SECTION)
            .and_then(|appearance| appearance.get(tilo_ui_base::theme::THEME_PACKAGE_KEY))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if package == self.bundled {
            return;
        }
        self.bundle = package
            .as_deref()
            .and_then(settings_sections::prefs::applied_theme_bundle);
        self.bundled = package;
    }

    /// Give every open window its appearance, and look at the temperatures
    /// when they are due (a workspace switch, a new wallpaper, or the few
    /// seconds between looks), with `person` sitting at this machine,
    /// `wallpapers` holding the pictures each workspace shows, and `network`
    /// what this device reads of the network.
    pub(crate) fn follow(
        &mut self,
        tabs: &mut AppTabs,
        handle: &DesktopHandle,
        wallpapers: &Wallpapers,
        person: &str,
        network: HostNetworkStatus,
        sync: Option<&BTreeMap<String, tilo_workspace_service::workspaces::sync::Workspace>>,
        now: Instant,
    ) {
        let theme_bundle = self.bundle.clone();
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
                    tilo_ui_base::theme::workspace_temperature_override(&settings, &id)
                        .or_else(|| wallpapers.hue(&id))
                });
            }
            // Each workspace's person is looked up once per pass, not once per window: the lookup
            // reads this device's identity registry.
            let persons = RefCell::new(HashMap::new());
            tabs.publish_window_warmth(
                |app| self.warmth.appearance_of(tree, app, &self.theme),
                |app, instance| workspace_sync(tree, app, instance, person, &persons, sync),
                &self.theme,
                theme_bundle.as_deref(),
                network,
            );
        });
    }
}

/// Where the workspace the window `instance` names stands with its peers: the window's own
/// workspace, never another window's. `None` falls back to `app`, for a connection that has not
/// named its window yet.
fn workspace_sync(
    tree: &Layout,
    app: &str,
    instance: Option<u64>,
    person: &str,
    persons: &RefCell<HashMap<WorkspaceId, String>>,
    sync: Option<&BTreeMap<String, tilo_workspace_service::workspaces::sync::Workspace>>,
) -> Option<WorkspaceSync> {
    let workspace = window_workspace(tree, app, instance)?;
    let workspace_id = workspace.id();
    let person = persons
        .borrow_mut()
        .entry(workspace_id)
        .or_insert_with(|| crate::identity::person_in(&workspace_id.to_string(), person))
        .clone();
    let member = workspace.policy.role_of(&person).is_some();
    let entry = sync.and_then(|sync| sync.get(&workspace_id.to_string()));
    let state = match entry.map(|entry| entry.status) {
        Some(tilo_workspace_service::workspaces::sync::Status::Connecting) => {
            WorkspaceSyncState::Connecting
        }
        Some(tilo_workspace_service::workspaces::sync::Status::Synced) => {
            WorkspaceSyncState::Synced
        }
        Some(tilo_workspace_service::workspaces::sync::Status::Unreachable) => {
            WorkspaceSyncState::Unreachable
        }
        // A workspace this device hosts, or one it keeps but not as a member, has no state of its
        // own: an app in it is told it is synced as far as it goes, as it always was.
        None if member => WorkspaceSyncState::Synced,
        None => return None,
    };
    Some(WorkspaceSync {
        state,
        workspace: workspace.name.clone(),
        unsynced_writes: entry.map_or(0, |entry| entry.unsynced_writes),
        cannot_keep_writes: false,
    })
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
/// overrides on top, built as the shell's own theme was when the session started. An appearance
/// that names an applied theme package wears that package's theme, loaded through the one bundle
/// loader, so its painter sets draw; a device without the package shows the mode's theme, as if
/// none were applied, and Settings says the package is missing.
fn shell_theme(document: &Map<String, Value>) -> Theme {
    let accessibility = ui::process::environment().accessibility;
    let applied = document
        .get("appearance")
        .and_then(|appearance| appearance.get("theme_package"))
        .and_then(Value::as_str)
        .and_then(settings_sections::prefs::applied_theme);
    Theme::configured_with(document, applied).with_a11y(
        &tilo_ui_base::a11y::A11ySettings {
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
        available: Some(link.kind != tilo_session_core::desktop::host::chrome::LinkKind::Offline),
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
            crate::sheets::set_theme(&theme);
            if let Some(shell) = self.shell.as_mut() {
                shell.set_theme(&theme);
            }
        }
        let Some(tabs) = &mut self.tabs else {
            return;
        };
        let network = network_status(&self.shared.readings);
        let sync = self
            .shared
            .workspace_sync
            .as_ref()
            .map(|reading| reading.get().workspaces);
        self.warmth.follow(
            tabs,
            &self.shared.handle,
            &self.shared.wallpapers,
            &self.shared.person,
            network,
            sync.as_ref(),
            ui::time::now(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::Role;
    use tilo_desktop_model::ids::{GroupId, StripId, TabId};
    use tilo_desktop_model::{Strip, Tab, TileContent, WindowGroup, WindowWidth, Workspace};

    const APP: &str = ui::apps::TASKS;
    const PERSON: &str = "ada";

    /// A workspace named `name` holding one window of [`APP`], where `PERSON` holds `role`.
    fn workspace(id: u64, name: &str, tab: TabId, role: Role) -> Workspace {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(u128::from(id)), name);
        if role == Role::Owner {
            workspace.policy.set_owner(PERSON);
        } else {
            workspace.policy.add_member(PERSON, role);
        }
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

    /// An app window in a workspace whose host has not been reached yet is told so, and told it is
    /// synced once the workspace's first sync completed: what the daemon publishes of the member
    /// copy is what the window shows.
    #[test]
    fn a_window_in_a_workspace_its_host_has_not_reached_is_told_connecting_then_synced() {
        use tilo_workspace_service::workspaces::sync::{Status, Workspace};

        let lounge = workspace(3, "Lounge", 300, Role::Owner);
        let tree = Layout {
            workspaces: vec![lounge],
            ..Layout::default()
        };
        let persons = RefCell::new(HashMap::from([(
            WorkspaceId::from_u128(3),
            PERSON.to_owned(),
        )]));
        let id = WorkspaceId::from_u128(3).to_string();
        let mut sync = BTreeMap::from([(
            id.clone(),
            Workspace {
                status: Status::Connecting,
                unsynced_writes: 0,
            },
        )]);
        let told = workspace_sync(&tree, APP, Some(300), PERSON, &persons, Some(&sync))
            .expect("the window is told where its workspace stands");
        assert_eq!(told.state, WorkspaceSyncState::Connecting);
        assert!(
            told.before_first_sync(),
            "an app shows it is connecting rather than an empty workspace"
        );

        sync.insert(
            id,
            Workspace {
                status: Status::Synced,
                unsynced_writes: 0,
            },
        );
        let told = workspace_sync(&tree, APP, Some(300), PERSON, &persons, Some(&sync))
            .expect("the window is told where its workspace stands");
        assert_eq!(told.state, WorkspaceSyncState::Synced);
        assert!(!told.before_first_sync(), "the workspace synced");
    }

    /// Two windows of one app, one in the person's own workspace and one in a shared one. Each
    /// window hears where its own workspace stands, never the other window's.
    #[test]
    fn a_window_is_told_its_workspace_never_another_windows() {
        let shared = workspace(2, "Shared", 200, Role::Viewer);
        let mine = workspace(1, "Mine", 100, Role::Owner);
        // The shared workspace stands first, as an app's other window does when the answer was
        // read from the app alone.
        let tree = Layout {
            workspaces: vec![shared, mine],
            ..Layout::default()
        };
        let persons = RefCell::new(HashMap::from([
            (WorkspaceId::from_u128(1), PERSON.to_owned()),
            (WorkspaceId::from_u128(2), PERSON.to_owned()),
        ]));
        let told = |instance| {
            workspace_sync(&tree, APP, Some(instance), PERSON, &persons, None)
                .map(|told| told.workspace)
        };
        assert_eq!(
            told(100).as_deref(),
            Some("Mine"),
            "the person's own window"
        );
        assert_eq!(
            told(200).as_deref(),
            Some("Shared"),
            "the shared workspace's window"
        );
        assert_eq!(
            told(999).as_deref(),
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
    /// `follow_settings` skips later passes. No store is no source installed, and no profile for
    /// the file a session leaves its settings in for the processes outside it: an empty home and
    /// no config root, which `source()` would otherwise answer through.
    #[test]
    fn without_a_settings_store_windows_follow_skips_reload() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", "")
            .remove("XDG_DATA_HOME")
            .remove("XDG_CONFIG_HOME");
        let _scope = settings_store::scoped_source(None);
        assert_eq!(settings_store::stamp(), None);

        let mut windows = Windows::new(Theme::light(), PERSON);
        assert!(
            windows.follow_settings(PERSON).is_none(),
            "a follow pass with still no stamp does not reload"
        );
    }
}
