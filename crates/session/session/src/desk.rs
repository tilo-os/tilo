//! The desk's own layer: the workspace's desk, the widgets on it, and the
//! choices those widgets draw from.
//!
//! The widgets sit on the wallpaper under the windows: the windows keep the
//! pointer, and whatever they do not take the desk still sees. What the
//! widgets show is the session's own: who is here, read from the desk's
//! tree, and today's date, read from the clock.
//!
//! Their preferences are the person's setting. The session reads them once
//! and keeps them current from the settings watch, so a frame draws from
//! memory and never reads the settings file.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tilo_desktop::desk_widgets::music::MusicAction as Action;
use tilo_desktop::desk_widgets::{DeskFold, DeskMusic};
use tilo_desktop::desktop_host::{DeskLayer, DesktopAsk};
use tilo_desktop::launchpad::CollaborationLaunchpad;
use tilo_lock::MutexRecover as _;
use tilo_quick_settings::QsEffect;
use tilo_surface_host::{DesktopHost, StripPolicy, desktop_with};
use ui::desk_widgets::DeskWidgetPrefs;

use crate::Options;
use crate::wallpapers::Wallpapers;

/// The desk widgets' choices as the session keeps them.
pub(crate) type Prefs = Arc<Mutex<DeskWidgetPrefs>>;

/// The person's widget choices as the settings document has them now.
pub(crate) fn prefs() -> Prefs {
    Arc::new(Mutex::new(DeskWidgetPrefs::from_document(
        &settings_store::load(),
    )))
}

/// What plays, as the desk's music widget shows it: the song, whether it
/// plays, and what its controls can do, from the panel's own reading of the
/// player.
pub(crate) fn playing(
    reading: &tilo_quick_settings::NowPlaying,
) -> tilo_desktop::desk_widgets::music::NowPlaying {
    tilo_desktop::desk_widgets::music::NowPlaying {
        title: reading.title.clone(),
        subtitle: reading.subtitle.clone(),
        player: reading.player_name.clone(),
        art: reading.art.clone(),
        playing: reading.playing,
        can_toggle: reading.can_toggle,
        can_previous: reading.can_previous,
        can_next: reading.can_next,
        position_ms: reading.position_ms,
        duration_ms: reading.duration_ms,
    }
}

impl crate::Shared {
    /// Steer the player the panel shows, as the panel's own controls do: the
    /// music widget's play or pause, previous and next are the panel's
    /// effects, carried out by what owns the players.
    pub(crate) fn music(&self, action: Action) {
        let Some(device) = &self.device else {
            return;
        };
        let effect = match action {
            Action::PlayPause => QsEffect::PlayPause,
            Action::Previous => QsEffect::PreviousTrack,
            Action::Next => QsEffect::NextTrack,
        };
        if let Err(error) = device.panel().run(effect) {
            tracing::debug!(%error, ?action, "the desk's music control did not reach the player");
        }
    }
}

/// The apps that run now, in the states the portal asks of them: the app
/// in front is in use, the ones behind it only run.
pub(crate) fn running_apps(compositor: &tilo_compositor::Compositor) -> Vec<(String, u32)> {
    use tilo_portal::background::{ACTIVE, RUNNING};
    let focused = compositor.focused_window().and_then(|window| window.app_id);
    let mut apps: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for (window, _) in compositor.windows() {
        let Some(app_id) = window.app_id else {
            continue;
        };
        let state = if focused.as_deref() == Some(app_id.as_str()) {
            ACTIVE
        } else {
            RUNNING
        };
        // An app showing more than one window is as in use as its best.
        let state = apps
            .get(&app_id)
            .copied()
            .map_or(state, |seen| seen.max(state));
        apps.insert(app_id, state);
    }
    apps.into_iter().collect()
}

/// Take the choices `document` carries into `prefs`, for the layer drawing
/// from them.
pub(crate) fn follow_prefs(prefs: &Prefs, document: &Map<String, Value>) {
    *prefs.lock_recover() = DeskWidgetPrefs::from_document(document);
}

/// The desk of this session's workspace and the policy placing its windows,
/// with what its tree asks of the session. The chrome above it asks through
/// the same list.
///
/// The widgets share `fold` with the session's keys, which fold an open card
/// on Escape, and read `overview` to fold it while the overview is over the
/// desk. `music` is what plays, as the session reads it from the player
/// Quick Settings shows.
pub(crate) fn desk(
    options: &Options,
    activity: &crate::activity::Activity,
    notices: tilo_desktop_surface::DesktopNotifications,
    pictures: Arc<Wallpapers>,
    widgets: Prefs,
    fold: DeskFold,
    overview: Rc<Cell<bool>>,
    music: DeskMusic,
) -> (DesktopHost, StripPolicy, Rc<RefCell<Vec<DesktopAsk>>>) {
    let asks = Rc::new(RefCell::new(Vec::new()));
    let (desktop, policy) = desktop_with(options.salt, options.scale, |mut surface, handle| {
        // The desk's windows ask it for the workspace's activity, and the
        // apps running beside it ask the same model through the broker.
        surface.set_history_source(Box::new(activity.clone()));
        surface.serve_activity(activity.requests());
        surface.set_notifications(notices.clone());
        let layer = tilo_desktop::desk_widgets::DeskWidgets::new(
            handle.clone(),
            options.person.clone(),
            Rc::clone(&asks),
            widgets,
        )
        .with_fold(fold)
        .with_overview(overview)
        .with_notices(notices)
        .with_music(music);
        let beneath = vec![Box::new(layer) as Box<dyn ui::widget::Widget>];
        Box::new(
            DeskLayer::new(
                Box::new(surface),
                handle.clone(),
                CollaborationLaunchpad::new(),
                beneath,
            )
            .with_asks(Rc::clone(&asks))
            .with_wallpaper(move |workspace| pictures.picture(workspace)),
        )
    });
    (desktop, policy, asks)
}
