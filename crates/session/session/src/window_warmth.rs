//! Window warmth: what the desk tells every app window about the room it
//! stands in.
//!
//! A window takes the temperature of the workspace it is shown in, the
//! colour of the person at this machine, the desk's theme, and where its
//! room stands with its peers. The desk works that out
//! ([`tilo_desktop_surface::warmth::WindowWarmth`]), this follows the
//! settings it comes from, and [`AppTabs::publish_window_warmth`] sends it
//! to each open window when it differs from what that window was told.
//!
//! The temperatures are looked at again every few seconds even while the
//! person stays in one workspace, so a wallpaper that arrives late or is
//! changed retints its room. The settings are read when they change, never
//! per frame.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

use serde_json::{Map, Value};
use tilo_desktop_model::DesktopTree;
use tilo_desktop_model::ids::RoomId;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::warmth::WindowWarmth;
use ui::Theme;
use ui::floating_surface::HostNetworkStatus;
use ui::tokens::PresenceHue;
use ui::workspace_sync::WorkspaceSync;

use crate::mount::app_tabs::AppTabs;
use crate::wallpapers::Wallpapers;

/// What every window is told of the desk it stands on, and the settings that
/// decide it as last read.
pub(crate) struct Windows {
    warmth: WindowWarmth,
    /// The theme windows are told: the desk's theme as the settings last
    /// made it.
    theme: Theme,
    /// The person's settings as last read: the workspace temperatures and
    /// their own colour come from it.
    document: Map<String, Value>,
    /// What version of the settings `document` is, so it is read again only
    /// when it changed.
    stamp: Option<settings_store::SettingsStamp>,
}

impl Windows {
    /// The windows of a desk wearing `theme`, with `person` sitting at this
    /// machine.
    pub(crate) fn new(theme: Theme, person: &str) -> Self {
        let document = settings_store::load();
        let your_colour = colour_in(&document, person);
        Self {
            warmth: WindowWarmth::new(your_colour),
            theme,
            document,
            stamp: settings_store::stamp(),
        }
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
        // A colour or theme chosen in Settings reaches the windows; nothing
        // reads the document on a pass where it did not change.
        let stamp = settings_store::stamp();
        if settings_store::document_changed(self.stamp.is_some(), self.stamp, stamp) {
            self.stamp = stamp;
            self.document = settings_store::load();
            self.theme = desk_theme(&self.document);
            self.warmth
                .set_your_colour(colour_in(&self.document, person));
        }
        handle.read(|tree| {
            let active = Some(tree.active);
            if self.warmth.due(active, now) {
                let settings = Value::Object(self.document.clone());
                let workspaces: Vec<_> = tree
                    .workspaces
                    .iter()
                    .map(|workspace| workspace.id)
                    .collect();
                self.warmth.update(&workspaces, active, now, &mut |id| {
                    let id = id.0.to_string();
                    ui::theme::workspace_temperature_override(&settings, &id)
                        .or_else(|| wallpapers.hue(&id))
                });
            }
            // Each room's person is looked up once per pass, not once per
            // window: the lookup reads this device's identity registry.
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

/// Where the room `app`'s window stands with its peers, and whether that
/// window may write it: the same answer that opened the app writable.
fn workspace_sync(
    tree: &DesktopTree,
    app: &str,
    person: &str,
    persons: &RefCell<HashMap<RoomId, String>>,
) -> Option<WorkspaceSync> {
    let workspace = tree.workspace_holding_app(app)?;
    let room = workspace.room();
    let person = persons
        .borrow_mut()
        .entry(room)
        .or_insert_with(|| crate::identity::person_in(&room.to_string(), person))
        .clone();
    tilo_network_actor::workspace_sync::told(
        tilo_network_actor::room_topic_id(room),
        &workspace.name,
        workspace.app_writes_at(&person, app, tilo_clock::unix_now()),
    )
}

/// The person's colour, as they sit at this machine: the one they chose in
/// Settings, else the one their identity derives — the same colour the
/// desk's own chip and every face of them wears.
fn colour_in(document: &Map<String, Value>, person: &str) -> PresenceHue {
    PresenceHue::chosen_in(&Value::Object(document.clone()))
        .unwrap_or_else(|| PresenceHue::of_identity(person))
}

/// The theme the desk and its windows wear: the settings document's, with
/// the host's accessibility overrides on top, built as the desk's own theme
/// was when the session started.
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

/// What this device reads of the network, as an app window may be told it:
/// whether it is on one, and nothing of which. `None` until the panel read
/// the device; the panel does not read whether the link is metered.
pub(crate) fn network_status(
    readings: &RefCell<Option<tilo_quick_settings::host::Feed>>,
) -> HostNetworkStatus {
    let feed = readings.borrow().clone();
    let Some(feed) = feed else {
        return HostNetworkStatus::default();
    };
    let state = feed.current();
    HostNetworkStatus {
        available: state
            .status
            .link
            .as_ref()
            .map(|link| link.kind != os_actions::LinkKind::Offline),
        metered: None,
    }
}
