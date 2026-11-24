//! The card of this device's call, over the windows while the person is not looking at the call:
//! its channel, whether this device is heard, and Mute, Deafen, Leave and Open (back to the call in
//! its app). An app draws only inside its window, so the system stands the card for every app's
//! call, from the call service's own state ([`tilo_native_media::NativeMediaService::own_call`]).
//!
//! The card is an activity on the live board, under [`APP`], beside every other activity the
//! shell draws: the bar's pill and quick settings list it, and its controls come back here. A
//! press acts on the call as the system ([`NativeMediaService::command_own`]), never as the app,
//! so the app's own next step is taken as it would have been; the app reads the change from its
//! watch of the call.

use runtime_collab::native_media::{Command, State};
use serde_json::Value;
use tilo_native_media::{NativeMediaService, OwnCall};
use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};

/// The board's name for the card: the system's, not any one app's.
pub(crate) const APP: &str = "so.tilo.calls";

/// What the card showed last, and the presses it took.
#[derive(Debug, Default)]
pub(crate) struct CallCard {
    /// The serial of the last press taken.
    pressed: u64,
    /// What the board was last told.
    shown: Option<Value>,
}

/// What the card's Open asks: the call's place, shown in the app the call is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Open {
    pub(crate) workspace: String,
    pub(crate) app: String,
    pub(crate) record: String,
    /// The place, by the address its app publishes it under (`tl:<app>:<id>`).
    pub(crate) resource: String,
}

impl CallCard {
    /// Take what was pressed on the card since the last pass, then show the call as it stands
    /// now: named `name`, on the board while `looking` says the person is not looking at it, and
    /// off it when there is no call. What Open asked, when it was pressed.
    pub(crate) fn pass(
        &mut self,
        board: &runtime_collab::LiveActivities,
        media: &NativeMediaService,
        name: &str,
        looking: impl FnOnce(&OwnCall) -> bool,
    ) -> Option<Open> {
        let mut open = None;
        if let Some((serial, controls)) = board.pressed_controls(APP, self.pressed) {
            self.pressed = serial;
            for control in controls {
                let Some(command) = control.get("command").and_then(Value::as_str) else {
                    continue;
                };
                let Some(call) = media.own_call() else {
                    continue;
                };
                if let Some(asked) = press(media, &call, command) {
                    open = Some(asked);
                }
            }
        }
        let activity = media
            .own_call()
            .filter(|call| open.is_none() && !looking(call))
            .and_then(|call| {
                serde_json::to_value(Published {
                    summary: Some(summary(&call, name)),
                    overlay: true,
                    announce: None,
                })
                .inspect_err(|error| tracing::warn!(%error, "the call card does not write"))
                .ok()
            });
        if activity != self.shown {
            self.shown.clone_from(&activity);
            board.show(APP, activity);
        }
        open
    }
}

/// Carry out the card's `command` on `call`: what Open asks, for Open.
fn press(media: &NativeMediaService, call: &OwnCall, command: &str) -> Option<Open> {
    let asked = match command {
        "mute" => Command::SetMuted { muted: true },
        "unmute" => Command::SetMuted { muted: false },
        "deafen" => Command::SetDeafened { deafened: true },
        "undeafen" => Command::SetDeafened { deafened: false },
        "leave" => Command::Leave,
        "open" => {
            return Some(Open {
                workspace: call.workspace.clone(),
                app: call.app.clone(),
                record: call.record.clone(),
                resource: tilolang_tilo::publish::resource_id(&call.app, &call.channel),
            });
        }
        other => {
            tracing::debug!(command = other, "a call card control nothing answers");
            return None;
        }
    };
    if let Err(error) = media.command_own(asked) {
        tracing::warn!(%error, command, "the call card's control did not reach the call");
    }
    None
}

/// The card of `call`, named `name`: where it stands, and its controls.
fn summary(call: &OwnCall, name: &str) -> Summary {
    let t = crate::i18n::t;
    let value = match call.state {
        State::Joining => t("call-card-joining"),
        State::Reconnecting => t("call-card-reconnecting"),
        _ if call.deafened => t("call-card-deafened"),
        _ if call.muted => t("call-card-muted"),
        _ => t("call-card-connected"),
    };
    let (mute, mute_label, mute_icon) = if call.muted {
        ("unmute", t("call-card-unmute"), "mic-off")
    } else {
        ("mute", t("call-card-mute"), "mic")
    };
    let (deafen, deafen_label, deafen_icon) = if call.deafened {
        ("undeafen", t("call-card-undeafen"), "headphone-off")
    } else {
        ("deafen", t("call-card-deafen"), "headphones")
    };
    let app = crate::app_names::app_name(&call.app);
    Summary::new(
        Kind::Call,
        if name.is_empty() {
            app.clone()
        } else {
            name.to_owned()
        },
        value,
    )
    .with_id(format!(
        "call:{}:{}:{}",
        call.workspace, call.app, call.channel
    ))
    .with_icon("phone")
    .with_source(app)
    .with_control(Control::new(mute, mute_label, ControlKind::Secondary).with_icon(mute_icon))
    .with_control(Control::new(deafen, deafen_label, ControlKind::Secondary).with_icon(deafen_icon))
    .with_control(
        Control::new("leave", t("call-card-leave"), ControlKind::Destructive)
            .with_icon("phone-off"),
    )
    .with_control(
        Control::new("open", t("call-card-open"), ControlKind::Primary).with_icon("maximize-2"),
    )
}

impl crate::Session {
    /// Stand the card of this device's call, named `name`, over the windows while none of its
    /// app's windows is in view, and carry out what was pressed on it.
    pub(crate) fn follow_call_card(&self, app_wire: &crate::app_wire::AppWire, name: &str) {
        let Some(media) = &app_wire.calls_service else {
            return;
        };
        let apps = self.shared.launch.as_ref();
        let looking = |call: &OwnCall| {
            let Some(in_view) = apps.and_then(|apps| apps.windows_in_view()) else {
                return false;
            };
            self.compositor.windows().into_iter().any(|(window, _)| {
                in_view.contains(&window.id)
                    && window
                        .app_id
                        .as_deref()
                        .is_some_and(|id| tilo_desktop_model::apps::app_ids_match(id, &call.app))
            })
        };
        let open = app_wire
            .call_card
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pass(self.shared.activities.board(), media, name, looking);
        if let (Some(open), Some(apps)) = (open, apps) {
            let reference = tilo_reference::ResourceRef::whole("", open.resource, "");
            if let Err(error) =
                apps.open_reference(&open.workspace, &open.app, Some(&open.record), &reference)
            {
                tracing::warn!(%error, "the call card could not open the call");
            }
        }
    }
}
