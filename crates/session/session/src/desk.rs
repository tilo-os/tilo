//! The desk's own layer: the workspace's desk, the widgets on it, and the choices those widgets
//! draw from.
//! The widgets sit on the wallpaper under the windows: the windows keep the pointer, and whatever
//! they do not take the desk still sees. What the widgets show is the session's own: who is here,
//! read from the desk's tree, and today's date, read from the clock.
//! Their preferences are the person's setting. The session reads them once and keeps them current
//! from the settings watch, so a frame draws from memory and never reads the settings file.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tilo_desktop::desk_widgets::music::MusicAction as Action;
use tilo_desktop::desk_widgets::{DeskFold, DeskMusic};
use tilo_desktop::desktop_host::{DeskLayer, DesktopAsk};
use tilo_desktop::launchpad::CollaborationLaunchpad;
use tilo_lock::MutexRecover as _;
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

impl crate::Shared {
    /// Steer the player in front, as the panel's own controls do: the music widget's play or pause,
    /// previous and next are the `media` resource's actions, which every caller reaches through the
    /// same door.
    pub(crate) fn music(&self, action: Action) {
        let action = match action {
            Action::PlayPause => "play-pause",
            Action::Previous => "previous",
            Action::Next => "next",
        };
        let address = tilo_actions::ActionAddress::new(crate::MEDIA, action);
        if let Err(error) = self.doors.invoke(
            &address,
            &serde_json::Map::new(),
            &mut tilo_surface_host::Look::none(),
        ) {
            tracing::warn!(%error, ?action, "the desk's music control did not reach the player");
        }
    }
}

/// The apps that run now, in the states the portal asks of them: the app in front is in use, the
/// ones behind it only run.
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

/// Take the choices `document` carries into `prefs` , for the layer drawing from them.
pub(crate) fn follow_prefs(prefs: &Prefs, document: &Map<String, Value>) {
    *prefs.lock_recover() = DeskWidgetPrefs::from_document(document);
}

/// What the desk's layer reads of the session: the person's desk widgets,
/// the fold the session's keys share, the overview's open flag, what plays,
/// and the bars' reveal the desk's band and policy follow.
pub(crate) struct DeskLook {
    pub(crate) widgets: Prefs,
    /// Shared with the session's keys, which fold an open card on Escape.
    pub(crate) fold: DeskFold,
    /// Whether the overview is over the desk, which folds it.
    pub(crate) overview: Rc<Cell<bool>>,
    /// What plays, as the session reads it from the player Quick Settings shows.
    pub(crate) music: DeskMusic,
    /// When each display's bar shows over the window covering it.
    pub(crate) reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>>,
}

/// The desk of this session's workspace and the policy placing its windows,
/// with what its tree asks of the session. The chrome above it asks through
/// the same list.
pub(crate) fn desk(
    options: &Options,
    activity: &crate::activity::Activity,
    notices: tilo_desktop_surface::DesktopNotifications,
    pictures: Arc<Wallpapers>,
    looked: DeskLook,
) -> (DesktopHost, StripPolicy, Rc<RefCell<Vec<DesktopAsk>>>) {
    let DeskLook {
        widgets,
        fold,
        overview,
        music,
        reveals,
    } = looked;
    let asks = Rc::new(RefCell::new(Vec::new()));
    let band_reveals = Rc::clone(&reveals);
    let (desktop, policy) = desktop_with(options.salt, options.scale, |mut surface, handle| {
        // The desk's windows ask it for the workspace's activity, and the apps running beside it
        // ask the same model through the broker.
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
                Rc::clone(&asks),
                beneath,
            )
            .with_wallpaper(move |workspace| pictures.picture(workspace))
            .with_reveals(Rc::clone(&band_reveals)),
        )
    });
    let mut policy = policy;
    policy.with_samples(Rc::new(RefCell::new(BarSamples { reveals })));
    // A tab opens with the name of its app in the person's language, never with the app's id.
    policy.reads_app_names(Rc::new(crate::app_names::app_name));
    (desktop, policy, asks)
}

/// The policy placing the desk's windows: the desktop's strips, or the wells of the shell drawing
/// the whole screen.
pub(crate) enum DeskPolicy {
    Strips(StripPolicy),
    Wells(tilo_surface_host::WellPolicy),
}

impl DeskPolicy {
    /// The shell drawing the whole screen places the windows in its wells; otherwise the desktop's
    /// strips do.
    pub(crate) fn for_shell(shell: bool, desktop: &DesktopHost, strips: StripPolicy) -> Self {
        if shell {
            Self::Wells(desktop.well_policy())
        } else {
            Self::Strips(strips)
        }
    }

    /// Tell `observer` the process of each window the desktop gives a place.
    pub(crate) fn set_window_mapped(&mut self, observer: Rc<dyn Fn(u32)>) {
        match self {
            Self::Strips(policy) => policy.set_window_mapped(observer),
            Self::Wells(policy) => policy.set_window_mapped(observer),
        }
    }

    /// Follow the windows the compositor opens and closes with `observer` .
    pub(crate) fn observe_windows(
        &mut self,
        observer: std::sync::Arc<dyn tilo_surface_host::WindowObserver>,
    ) {
        match self {
            Self::Strips(policy) => policy.observe_windows(observer),
            Self::Wells(policy) => policy.observe_windows(observer),
        }
    }

    /// The wells the shell's views report to, when they place the windows.
    pub(crate) fn wells(&self) -> Option<ui::widget::Wells> {
        match self {
            Self::Strips(_) => None,
            Self::Wells(policy) => Some(policy.wells()),
        }
    }

    /// The policy, as the compositor runs with it.
    pub(crate) fn boxed(self) -> Box<dyn tilo_compositor::WindowPolicy> {
        match self {
            Self::Strips(policy) => Box::new(policy),
            Self::Wells(policy) => Box::new(policy),
        }
    }
}

/// What the desk's policy sends the bars' reveal: the pointer wherever it goes, presses, and
/// fingers on the screen, which the chrome follows beyond where it draws.
struct BarSamples {
    reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>>,
}

impl tilo_surface_host::PointerSamples for BarSamples {
    fn moved(&mut self, x: f64, y: f64) {
        let now = ui::time::now();
        self.reveals
            .borrow_mut()
            .motion(ui::vello::kurbo::Point::new(x, y), now);
    }

    fn pressed(&mut self, x: f64, y: f64) {
        self.reveals
            .borrow_mut()
            .press(ui::vello::kurbo::Point::new(x, y));
    }

    fn touch(&mut self, touch: tilo_compositor::TouchSample) -> bool {
        use tilo_desktop::topbar::Finger;
        let now = ui::time::now();
        let mut reveals = self.reveals.borrow_mut();
        let (finger, x, y) = match touch {
            tilo_compositor::TouchSample::Down { x, y } => (Finger::Down, x, y),
            tilo_compositor::TouchSample::Move { x, y } => (Finger::Move, x, y),
            tilo_compositor::TouchSample::Lift { x, y } => (Finger::Lift, x, y),
            tilo_compositor::TouchSample::Cancel => (Finger::Cancel, 0.0, 0.0),
        };
        reveals.touch(finger, ui::vello::kurbo::Point::new(x, y), now)
    }
}
