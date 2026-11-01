//! What quick settings asks the session to do, and where it lands.
//!
//! The panel's sections take most of their effects on their own; what only
//! the desktop can do — closing the panel, locking, the notification
//! centre, a device's page in Settings — the device host queues as
//! [`SessionRequest`]s, and this answers them. A request no section here
//! answers goes on to `mount`'s own modules, and one nothing answers is
//! logged rather than dropped without trace.

use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_desktop::overlays::Slot;
use tilo_desktop_surface::navigation::ChromeEffect;
use tilo_quick_settings::QsEffect;
use tilo_quick_settings::device::{DeviceHost, SessionRequest};

use crate::{SESSION_RESOURCE, Shared, notifications};

/// The settings app, which quick settings opens.
const SETTINGS_APP: &str = "so.tilo.settings";

/// Where Settings installs the local speech models dictation needs: the
/// page of the models the person's devices run themselves.
const SPEECH_MODELS_ANCHOR: &str = "settings.section:agent_models";

/// Close the panel: what a press that takes the person elsewhere does first.
pub(crate) fn close(shared: &Shared) {
    shared.overlays.close(Slot::QuickSettings);
}

/// Take the person where the speech models are set up when the dictation
/// card's "Set up" was pressed: Settings at its models page, through the
/// action path search rows use; a plain launch opens Settings too when no
/// action host runs an app's actions.
pub(crate) fn follow_dictation(shared: &Shared) {
    if !shared.dictation.take_model_setup() {
        return;
    }
    open_settings(shared, SPEECH_MODELS_ANCHOR);
}

/// Open Settings at the page `link` names — a `settings://` page or a page's
/// anchor — through the action path search rows run; a plain launch opens
/// Settings itself too when no action host runs an app's actions.
pub(crate) fn open_settings(shared: &Shared, link: &str) {
    close(shared);
    let room = shared.active_room();
    let anchor = match link.strip_prefix("settings://") {
        Some(slug) => Some(format!("settings.section:{slug}")),
        None => (!link.is_empty()).then(|| link.to_owned()),
    };
    if let Some(anchor) = anchor {
        let target = tilo_desktop_search::Target::Invoke(Box::new(
            tilo_desktop_search::InvokeTarget::open(SETTINGS_APP, room.clone(), anchor),
        ));
        let opened = shared.search.perform_checked(&room, target, &mut |own| {
            shared.doors.invoke(&own.address, &own.arguments).map(drop)
        });
        if opened.is_ok() {
            return;
        }
    }
    let launch = ChromeEffect::OpenApp {
        app: SETTINGS_APP.into(),
        beside: None,
        background: None,
    };
    if let Err(error) = shared.handle.dispatch(launch) {
        tracing::warn!(%error, "open settings");
    }
}

/// Take in every reading the panel shows from the service that publishes
/// it, then let the panel take them in.
fn land(shared: &Shared, device: &DeviceHost) {
    // The audio service's reading is the only one of the sound: the panel
    // shows it and its effects change it there.
    super::audio::land(shared);
    // What plays is published by the same service, from the players it
    // follows: the card shows that reading too.
    super::media::land(shared);
    device.pump();
    super::devices::land(shared);
    super::displays::land(shared);
    super::panel_power::land(shared);
    super::panel_system::land(shared);
}

/// Land what the device host read and carry out what the panel asked of the
/// desktop.
pub(crate) fn answer(shared: &Shared) {
    let Some(device) = &shared.device else {
        return;
    };
    device.set_open(shared.overlays.is_open(Slot::QuickSettings));
    land(shared, device);
    for request in device.take_requests() {
        match request {
            // Putting the menu up, locking and opening a kept notification
            // are the session's own verbs, published beside it: the panel
            // invokes them as any other caller would.
            SessionRequest::Effect(QsEffect::PowerMenu) => {
                close(shared);
                invoke(shared, SESSION_RESOURCE, "power-menu", &Map::new());
            }
            SessionRequest::Effect(QsEffect::LockScreen) => {
                close(shared);
                invoke(shared, SESSION_RESOURCE, "lock", &Map::new());
            }
            SessionRequest::Effect(QsEffect::OpenNotification { notification }) => {
                close(shared);
                let mut arguments = Map::new();
                arguments.insert("notification".into(), Value::from(notification));
                invoke(shared, notifications::RESOURCE, "open", &arguments);
            }
            SessionRequest::Effect(QsEffect::SetDoNotDisturb { on }) => {
                // The switch is persisted, so the settings app and the
                // desktop's own menu read the same one; the session's
                // settings watch brings it back into every arrival.
                if let Err(error) = shared.notifications.borrow().set_do_not_disturb(on) {
                    tracing::warn!(%error, on, "do not disturb not saved");
                }
            }
            SessionRequest::Effect(QsEffect::OpenSettings { link }) => {
                open_settings(shared, &link);
            }
            // A copy is pasted into the app the panel was opened over, so the
            // panel closes and the loop puts it on the clipboard and pastes
            // it once the app has the keyboard again.
            SessionRequest::Effect(QsEffect::PasteCopy { copy }) => {
                close(shared);
                shared.clipboard.ask(super::clipboard::Ask::Paste(copy));
            }
            SessionRequest::Effect(QsEffect::PinCopy { copy, pinned }) => {
                shared.clipboard.pin(&copy, pinned);
            }
            SessionRequest::Effect(QsEffect::RemoveCopy { copy }) => {
                shared.clipboard.remove(&copy);
            }
            SessionRequest::Effect(QsEffect::ClearClipboard) => {
                shared.clipboard.clear();
            }
            other if super::audio::follow(shared, &other) => {}
            other if super::media::follow(shared, &other) => {}
            other if super::displays::follow(shared, &other) => {}
            other if super::devices::follow(shared, &other) => {}
            other if follow_centre(shared, &other) => {}
            other => {
                tracing::debug!(?other, "quick settings asks what the session cannot do yet");
            }
        }
    }
    shared.feed_notifications();
    shared.feed_clipboard();
}

/// Carry out what the panel asked of a notification kept in the centre: the
/// centre's own verbs, which the panel reads the result of in the same
/// reading. Whether this was one of them; `false` leaves it to the rest of
/// what the session follows.
fn follow_centre(shared: &Shared, request: &SessionRequest) -> bool {
    let SessionRequest::Effect(effect) = request else {
        return false;
    };
    let mut arguments = Map::new();
    let action = match effect {
        QsEffect::DismissNotification { notification } => {
            arguments.insert("notification".into(), Value::from(*notification));
            "dismiss"
        }
        QsEffect::DismissNotificationGroup { group } => {
            arguments.insert("group".into(), Value::from(group.clone()));
            "dismiss"
        }
        QsEffect::RunNotificationAction {
            notification,
            action,
        } => {
            arguments.insert("notification".into(), Value::from(*notification));
            arguments.insert("action".into(), Value::from(action.clone()));
            "act"
        }
        QsEffect::ClearNotifications => "clear",
        QsEffect::UndoClearNotifications => "undo-clear",
        _ => return false,
    };
    invoke(shared, notifications::RESOURCE, action, &arguments);
    true
}

/// Ask one of the session's own resources to carry out `action`, as the
/// panel's press does: through the door every caller uses, with its
/// arguments. A refusal is the resource's own words, and what the panel
/// shows is its reading, so it changes on the next pass either way.
fn invoke(shared: &Shared, resource: &str, action: &str, arguments: &Map<String, Value>) {
    let address = ActionAddress::new(resource, action);
    if let Err(error) = shared.doors.invoke(&address, arguments) {
        tracing::debug!(%error, %action, "the panel's press did not reach the session");
    }
}
