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

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
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

/// Take the choices `document` carries into `prefs`, for the layer drawing
/// from them.
pub(crate) fn follow_prefs(prefs: &Prefs, document: &Map<String, Value>) {
    *prefs.lock_recover() = DeskWidgetPrefs::from_document(document);
}

/// The desk of this session's workspace and the policy placing its windows,
/// with what its tree asks of the session. The chrome above it asks through
/// the same list.
pub(crate) fn desk(
    options: &Options,
    notices: tilo_desktop_surface::DesktopNotifications,
    pictures: Arc<Wallpapers>,
    widgets: Prefs,
) -> (DesktopHost, StripPolicy, Rc<RefCell<Vec<DesktopAsk>>>) {
    let asks = Rc::new(RefCell::new(Vec::new()));
    let (desktop, policy) = desktop_with(options.salt, options.scale, |mut surface, handle| {
        surface.set_notifications(notices);
        let beneath = vec![Box::new(tilo_desktop::desk_widgets::DeskWidgets::new(
            handle.clone(),
            options.person.clone(),
            Rc::clone(&asks),
            widgets,
        )) as Box<dyn ui::widget::Widget>];
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
