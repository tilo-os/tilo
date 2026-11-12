//! Window warmth: what the desk tells every app window about the workspace
//! it stands in.
//!
//! A window takes the temperature of the workspace it is shown in, the
//! colour of the person at this machine, the desk's theme, and where its
//! workspace stands with its peers. The desk works that out
//! ([`tilo_desktop_surface::warmth::WindowWarmth`]), this follows the
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
use tilo_desktop_model::DesktopTree;
use tilo_desktop_model::ids::WorkspaceId;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::warmth::WindowWarmth;
use ui::Theme;
use ui::floating_surface::HostNetworkStatus;
use ui::tokens::PresenceHue;
use ui::workspace_sync::WorkspaceSync;

use crate::app_tabs::AppTabs;
use crate::wallpapers::Wallpapers;

/// What every window is told of the desk it stands on, and the settings that decide it as last
/// read.
pub(crate) struct Windows {
    warmth: WindowWarmth,
    /// The theme windows are told: the desk's theme as the settings last made it.
    theme: Theme,
    /// The person's settings as last read: the workspace temperatures and their own colour come
    /// from it.
    document: Map<String, Value>,
    /// What version of the settings `document` is, so it is read again only when it changed.
    stamp: Option<settings_store::SettingsStamp>,
}

impl Windows {
    /// The windows of a desk wearing `theme` , with `person` sitting at this machine.
    pub(crate) fn new(theme: Theme, person: &str) -> Self {
        let document = settings_store::load();
        let your_colour = PresenceHue::worn_in(&Value::Object(document.clone()), person);
        Self {
            warmth: WindowWarmth::new(your_colour),
            theme,
            document,
            stamp: settings_store::stamp(),
        }
    }

    /// Read the person's settings again when they changed, with `person` sitting at this machine:
    /// the desk's theme as the changed settings make it (dark mode in the quick settings, a theme
    /// chosen in Settings), for everything the session draws to wear. Nothing reads the document on
    /// a pass where it did not change.
    pub(crate) fn follow_settings(&mut self, person: &str) -> Option<Theme> {
        // The document was read when the desk started (`new`), so only a stamp that moved since
        // says it changed.
        let stamp = settings_store::stamp();
        if !settings_store::document_changed(true, self.stamp, stamp) {
            return None;
        }
        self.stamp = stamp;
        self.document = settings_store::load();
        self.warmth.set_your_colour(PresenceHue::worn_in(
            &Value::Object(self.document.clone()),
            person,
        ));
        self.theme = desk_theme(&self.document);
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
                    .map(|workspace| workspace.slot)
                    .collect();
                self.warmth.update(&workspaces, active, now, &mut |id| {
                    let id = id.0.to_string();
                    ui::theme::workspace_temperature_override(&settings, &id)
                        .or_else(|| wallpapers.hue(&id))
                });
            }
            // Each workspace's person is looked up once per pass, not once per window: the lookup
            // reads this device's identity registry.
            let persons = RefCell::new(HashMap::new());
            tabs.publish_window_warmth(
                |app| self.warmth.appearance_of(tree, app, &self.theme),
                |app| workspace_sync(tree, app, person, &persons),
                &self.theme,
                network,
            );
        });
    }
}

/// Where the workspace `app` 's window stands with its peers, and whether that window may write it:
/// the same answer that opened the app writable.
fn workspace_sync(
    tree: &DesktopTree,
    app: &str,
    person: &str,
    persons: &RefCell<HashMap<WorkspaceId, String>>,
) -> Option<WorkspaceSync> {
    let workspace = tree.workspace_holding_app(app)?;
    let workspace_id = workspace.id();
    let person = persons
        .borrow_mut()
        .entry(workspace_id)
        .or_insert_with(|| crate::identity::person_in(&workspace_id.to_string(), person))
        .clone();
    tilo_network_actor::workspace_sync::told(
        tilo_network_actor::workspace_topic_id(workspace_id),
        &workspace.name,
        workspace.app_writes_at(&person, app, tilo_clock::unix_now()),
    )
}

/// The theme the desk and its windows wear: the settings document's, with the host's accessibility
/// overrides on top, built as the desk's own theme was when the session started.
fn desk_theme(document: &Map<String, Value>) -> Theme {
    let accessibility = ui::host::environment().accessibility;
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
        // A theme the person's settings changed to is what the desk, the chrome above the windows
        // (the search palette among it) and the shell draw in from this frame, as every window is
        // told it below.
        if let Some(theme) = self.warmth.follow_settings(&self.shared.person) {
            self.shared.desktop.set_theme(theme.clone());
            self.shared.chrome.borrow_mut().set_theme(theme.clone());
            self.shared.people.borrow().set_theme(theme.clone());
            self.readout.borrow_mut().set_theme(theme.clone());
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
