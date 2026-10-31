//! What quick settings asks the session to do, and where it lands.
//!
//! The panel's sections take most of their effects on their own; what only
//! the desktop can do — closing the panel, locking, the notification
//! centre, a device's page in Settings — the device host queues as
//! [`SessionRequest`]s, and this answers them. A request no section here
//! answers goes on to `mount`'s own modules, and one nothing answers is
//! logged rather than dropped without trace.

use tilo_desktop_surface::navigation::Command;
use tilo_quick_settings::QsEffect;
use tilo_quick_settings::device::SessionRequest;
use tilo_sheets::Seat as _;

use crate::{GENERATION, SessionCommand, Shared};

/// The settings app, which quick settings opens.
const SETTINGS_APP: &str = "so.tilo.settings";

/// Where Settings installs the local speech models dictation needs: the
/// page of the models the person's devices run themselves.
const SPEECH_MODELS_ANCHOR: &str = "settings.section:agent_models";

/// Close the panel: what a press that takes the person elsewhere does first.
pub(crate) fn close(shared: &Shared) {
    if let Some(panel) = &shared.quick_settings {
        panel.open.set(false);
    }
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
    let launch = Command::Launch {
        generation: GENERATION.into(),
        app: SETTINGS_APP.into(),
    };
    if let Err(error) = shared.handle.dispatch(&launch, GENERATION) {
        tracing::warn!(%error, "open settings");
    }
}

/// Land what the device host read and carry out what the panel asked of the
/// desktop.
pub(crate) fn answer(shared: &Shared) {
    let Some(device) = &shared.device else {
        return;
    };
    device.set_open(
        shared
            .quick_settings
            .as_ref()
            .is_some_and(|panel| panel.open.get()),
    );
    // The audio service's reading is the only one of the sound: the panel
    // shows it and its effects change it there.
    super::audio::land(shared);
    device.pump();
    super::devices::land(shared);
    for request in device.take_requests() {
        match request {
            SessionRequest::Effect(QsEffect::PowerMenu) => {
                close(shared);
                if let Some(power) = &shared.power {
                    power.presentation().open(false);
                }
            }
            SessionRequest::Effect(QsEffect::LockScreen) => {
                close(shared);
                shared.commands.borrow_mut().push(SessionCommand::Lock);
            }
            SessionRequest::Effect(QsEffect::OpenNotification { notification }) => {
                close(shared);
                let room = shared.active_room();
                match shared.notifications.borrow().open(notification) {
                    tilo_desktop_surface::notifications::HistoryActivation::OpenApp {
                        app_id,
                        ..
                    } => shared.search.perform(
                        &room,
                        tilo_desktop_search::Target::Launch { app: app_id },
                        &mut |own| shared.doors.invoke(&own.address, &own.arguments).map(drop),
                    ),
                    other => tracing::debug!(?other, "notification chosen"),
                }
            }
            SessionRequest::Effect(QsEffect::DismissNotification { notification }) => {
                shared
                    .notifications
                    .borrow()
                    .dismiss(Some(notification), None);
            }
            SessionRequest::Effect(QsEffect::DismissNotificationGroup { group }) => {
                shared.notifications.borrow().dismiss(None, Some(&group));
            }
            SessionRequest::Effect(QsEffect::RunNotificationAction {
                notification,
                action,
            }) => {
                shared
                    .notifications
                    .borrow()
                    .run_action(notification, &action);
            }
            SessionRequest::Effect(QsEffect::ClearNotifications) => {
                shared.notifications.borrow().clear();
            }
            SessionRequest::Effect(QsEffect::UndoClearNotifications) => {
                shared.notifications.borrow().undo_clear();
            }
            SessionRequest::Effect(QsEffect::SetDoNotDisturb { on }) => {
                // The switch is persisted, so the settings app and the
                // desktop's own menu read the same one; the session's
                // settings watch brings it back into every arrival.
                if let Err(error) =
                    crate::notifications::SessionNotifications::set_do_not_disturb(on)
                {
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
            other if super::devices::follow(shared, &other) => {}
            other => {
                tracing::debug!(?other, "quick settings asks what the session cannot do yet");
            }
        }
    }
    // What plays reaches the desk from the same reading the panel shows, so
    // the widget and the panel never disagree and neither reads the players
    // again.
    let playing = device.state().now_playing;
    shared.music.set(playing.as_ref().map(crate::desk::playing));
    shared.feed_notifications();
    shared.feed_clipboard();
}
