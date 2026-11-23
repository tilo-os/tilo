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
use tilo_desktop_model::Layout;
use tilo_desktop_model::ids::WorkspaceId;
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
    stamp: Option<settings_store::SettingsStamp>,
}

impl Windows {
    /// The windows of a shell wearing `theme` , with `person` sitting at this machine.
    pub(crate) fn new(theme: Theme, person: &str) -> Self {
        let document = settings_store::load();
        let your_colour = PresenceHue::worn_in(&Value::Object(document.clone()), person);
        Self {
            warmth: WindowWarmth::new(your_colour),
            theme: theme.with_your_colour(your_colour),
            document,
            stamp: settings_store::stamp(),
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
        // The document was read when the shell started (`new`), so only a stamp that moved since
        // says it changed.
        let stamp = settings_store::stamp();
        if !settings_store::document_changed(true, self.stamp, stamp) {
            return None;
        }
        self.stamp = stamp;
        self.document = settings_store::load();
        let your_colour = PresenceHue::worn_in(&Value::Object(self.document.clone()), person);
        self.warmth.set_your_colour(your_colour);
        self.theme = shell_theme(&self.document).with_your_colour(your_colour);
        Some(self.theme.clone())
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
            // Each workspace's person is looked up once per pass, not once per window: the lookup
            // reads this device's identity registry.
            let persons = RefCell::new(HashMap::new());
            tabs.publish_window_warmth(
                |app| self.warmth.appearance_of(tree, app, &self.theme),
                |app, instance| workspace_sync(tree, app, instance, person, &persons),
                &self.theme,
                network,
            );
        });
    }
}

/// Where the workspace the window `instance` names stands with its peers, and whether that
/// window may write it: the same answer that opened the app writable.
///
/// The window's own workspace answers, never another window's: an app may have a window in a
/// workspace its person writes and another in one they only read, and being told the other's
/// answer shows "View only" over a record they may edit. `None` falls back to `app`, for a
/// connection that has not named its window yet.
fn workspace_sync(
    tree: &Layout,
    app: &str,
    instance: Option<u64>,
    person: &str,
    persons: &RefCell<HashMap<WorkspaceId, String>>,
) -> Option<WorkspaceSync> {
    let workspace = window_workspace(tree, app, instance)?;
    let workspace_id = workspace.id();
    let person = persons
        .borrow_mut()
        .entry(workspace_id)
        .or_insert_with(|| crate::identity::person_in(&workspace_id.to_string(), person))
        .clone();
    tilo_network_actor::workspace_sync::told(
        workspace_id,
        &workspace.name,
        workspace.app_writes_at(&person, app, tilo_clock::unix_now()),
    )
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
            &self.shared.person,
            network,
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
            workspace.set_owner(PERSON);
        } else {
            workspace.admit(PERSON, role);
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

    /// Two windows of one app: the person writes in their own workspace and only reads in a
    /// shared one. Each window hears what its own workspace says — an agent that opened the app
    /// in the shared workspace is no reason for the person's own window to read "View only".
    #[test]
    fn a_window_is_told_its_own_workspaces_answer_never_another_windows() {
        let shared = workspace(2, "Shared", 200, Role::Viewer);
        let mine = workspace(1, "Mine", 100, Role::Owner);
        // The shared workspace stands first, as an app's other window does when the answer was
        // read from the app alone: the person's own window reads "View only" over what they may
        // edit.
        let tree = Layout {
            workspaces: vec![shared, mine],
            ..Layout::default()
        };
        let persons = RefCell::new(HashMap::from([
            (WorkspaceId::from_u128(1), PERSON.to_owned()),
            (WorkspaceId::from_u128(2), PERSON.to_owned()),
        ]));
        let writes = |instance| {
            workspace_sync(&tree, APP, Some(instance), PERSON, &persons).map(|told| told.writable)
        };
        assert_eq!(
            writes(100),
            Some(Some(true)),
            "the person's own window writes where they do"
        );
        assert_eq!(
            writes(200),
            Some(Some(false)),
            "the shared workspace's window is told it may only read"
        );
        assert_eq!(
            writes(999),
            Some(Some(false)),
            "a connection that named no window falls back to the first workspace holding the app"
        );
    }
}
