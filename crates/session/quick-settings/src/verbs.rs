//! What the panel's effects are, as the session's published actions.
//!
//! Every switch, slider and row of the panel is an effect ([`QsEffect`]), and
//! every effect that changes the device is one action of one of the session's
//! resources: the connectivity service's network, Bluetooth adapter and VPN
//! profiles, the audio service's sound and players, the session's displays,
//! notification centre, clipboard and person's devices, and the session's own
//! screen and night light. [`of`] says which, and with what.
//!
//! The effects the panel carries out itself — what it shows, what it keeps,
//! the prompts it opens — are not here: they change no device, so no resource
//! hears of them ([`crate::live::Live::local`]).

use ui::serde_json::{self, Map, Value};

use crate::QuickSettingsState;
use crate::effect::QsEffect;
use crate::session::{
    BLUETOOTH, CHROME, CLIPBOARD, DESK, DEVICES, DISPLAY_SETTINGS, DISPLAYS, MEDIA, NETWORK,
    NIGHT_LIGHT, NOTIFICATIONS, SCREEN, SLEEP, SOUND, VPN,
};

/// What an effect asks of the session: the resource, its action and the
/// arguments the action takes.
pub(crate) type Verb = (&'static str, &'static str, Map<String, Value>);

/// The resource and action `effect` is, with its arguments; `None` for an
/// effect the panel carries out itself, or one no resource answers.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per effect the panel has, in the order its sections show them"
)]
pub(crate) fn of(state: &QuickSettingsState, effect: &QsEffect) -> Option<Verb> {
    let mut arguments = Map::new();
    let (resource, action) = match effect {
        // The radios, the links and the networks.
        QsEffect::SetWifi { on } => {
            put(&mut arguments, "on", on);
            (NETWORK, "set-wifi")
        }
        QsEffect::ToggleWifi => (NETWORK, "toggle-wifi"),
        QsEffect::SetWired { on } => {
            put(&mut arguments, "on", on);
            (NETWORK, "set-wired")
        }
        QsEffect::SetAirplaneMode { on } => {
            put(&mut arguments, "on", on);
            (NETWORK, "set-airplane-mode")
        }
        QsEffect::ToggleAirplaneMode => (NETWORK, "toggle-airplane-mode"),
        QsEffect::SetMobileData { on } => {
            put(&mut arguments, "on", on);
            (NETWORK, "set-mobile-data")
        }
        QsEffect::JoinWifi { network } => {
            put(&mut arguments, "network", network);
            (NETWORK, "join-wifi")
        }
        QsEffect::ConnectWifi { network, password } => {
            put(&mut arguments, "network", network);
            put(&mut arguments, "password", password);
            (NETWORK, "connect-wifi")
        }
        QsEffect::ConnectHiddenWifi { network, password } => {
            put(&mut arguments, "network", network);
            arguments.insert("password".into(), Value::from(password.clone()));
            (NETWORK, "connect-hidden-wifi")
        }
        QsEffect::SetNetworkAutoconnect { profile, on } => {
            put(&mut arguments, "profile", profile);
            put(&mut arguments, "on", on);
            (NETWORK, "set-network-autoconnect")
        }
        QsEffect::ForgetNetwork { profile } => {
            put(&mut arguments, "profile", profile);
            (NETWORK, "forget-network")
        }
        // The Bluetooth adapter.
        QsEffect::SetBluetooth { on } => {
            put(&mut arguments, "on", on);
            (BLUETOOTH, "set-bluetooth")
        }
        QsEffect::ToggleBluetooth => (BLUETOOTH, "toggle-bluetooth"),
        QsEffect::ConnectBluetooth { address } => {
            put(&mut arguments, "address", address);
            (BLUETOOTH, "connect")
        }
        QsEffect::DisconnectBluetooth { address } => {
            put(&mut arguments, "address", address);
            (BLUETOOTH, "disconnect")
        }
        QsEffect::ScanBluetooth { on } => {
            put(&mut arguments, "on", on);
            (BLUETOOTH, "scan")
        }
        QsEffect::PairBluetooth { address } => {
            put(&mut arguments, "address", address);
            (BLUETOOTH, "pair")
        }
        QsEffect::ForgetBluetooth { address } => {
            put(&mut arguments, "address", address);
            (BLUETOOTH, "forget")
        }
        // The VPN profiles.
        QsEffect::SetVpn { profile, on } => {
            put(&mut arguments, "profile", profile);
            put(&mut arguments, "on", on);
            (VPN, "set-vpn")
        }
        // The displays.
        QsEffect::SetBrightness { level } => {
            put(&mut arguments, "level", level);
            (DISPLAYS, "set-brightness")
        }
        QsEffect::SetDisplayBrightness { display, level } => {
            put(&mut arguments, "display", display);
            put(&mut arguments, "level", level);
            (DISPLAYS, "set-display-brightness")
        }
        QsEffect::BrightnessUp => (DISPLAYS, "brightness-up"),
        QsEffect::BrightnessDown => (DISPLAYS, "brightness-down"),
        // The sound.
        QsEffect::SetVolume { level } => {
            put(&mut arguments, "level", level);
            (SOUND, "set-volume")
        }
        QsEffect::VolumeUp => (SOUND, "volume-up"),
        QsEffect::VolumeDown => (SOUND, "volume-down"),
        QsEffect::SetMuted { muted } => {
            put(&mut arguments, "muted", muted);
            (SOUND, "set-muted")
        }
        QsEffect::ToggleMuted => (SOUND, "toggle-muted"),
        QsEffect::RouteSound { node } => {
            put(&mut arguments, "node", node);
            (SOUND, "route-output")
        }
        QsEffect::ChooseOutput { output } => {
            // The card names a destination by the resource that stands for
            // it; this machine's outputs are the audio service's.
            let node = audio::sound::output_node(output)?;
            put(&mut arguments, "node", &node);
            (SOUND, "route-output")
        }
        QsEffect::OpenPlaying { app } => {
            put(&mut arguments, "app", app);
            (CHROME, "show-app")
        }
        QsEffect::SetInputVolume { level } => {
            put(&mut arguments, "level", level);
            (SOUND, "set-input-volume")
        }
        QsEffect::SetInputMuted { muted } => {
            put(&mut arguments, "muted", muted);
            (SOUND, "set-input-muted")
        }
        QsEffect::ToggleInputMuted => (SOUND, "toggle-input-muted"),
        QsEffect::RouteStream { stream, device } => {
            put(&mut arguments, "stream", stream);
            arguments.insert("device".into(), Value::from(*device));
            (SOUND, "route-stream")
        }
        QsEffect::SetStreamVolume { stream, level } => {
            put(&mut arguments, "stream", stream);
            put(&mut arguments, "level", level);
            (SOUND, "set-stream-volume")
        }
        QsEffect::SetStreamMuted { stream, muted } => {
            put(&mut arguments, "stream", stream);
            put(&mut arguments, "muted", muted);
            (SOUND, "set-stream-muted")
        }
        // What plays.
        QsEffect::PlayPause => (MEDIA, "play-pause"),
        QsEffect::PreviousTrack => (MEDIA, "previous"),
        QsEffect::NextTrack => (MEDIA, "next"),
        QsEffect::Seek { fraction } => {
            put(&mut arguments, "fraction", fraction);
            (MEDIA, "seek")
        }
        QsEffect::ResumePlayback { request } => {
            // What the other device played, as the card holds it: only a
            // stream or a file this device can open itself is a hand-over,
            // which the card refuses the rest of.
            let offer = state
                .offer
                .as_ref()
                .filter(|offer| offer.request_id == *request)?;
            put(&mut arguments, "request", &offer.request_id);
            put(&mut arguments, "app", &offer.package_id);
            match &offer.source {
                media::handoff::HandoffSource::RemoteStream { url } => {
                    put(&mut arguments, "url", url);
                }
                media::handoff::HandoffSource::LocalFile { path } => {
                    put(&mut arguments, "path", &path.to_string_lossy());
                }
                media::handoff::HandoffSource::Workspace { .. }
                | media::handoff::HandoffSource::AppResource { .. } => return None,
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a playhead in a recording is far inside u64's milliseconds"
            )]
            let position_ms = offer.position_ms.max(0.0).round() as u64;
            put(&mut arguments, "position_ms", &position_ms);
            put(&mut arguments, "playing", &offer.playing);
            (MEDIA, "resume")
        }
        // The notification centre: the panel's row opens the one list, on
        // the desk, rather than a page of its own.
        QsEffect::ShowCentre => {
            put(&mut arguments, "open", &true);
            (CHROME, "notifications")
        }
        QsEffect::SetDoNotDisturb { on } => {
            put(&mut arguments, "on", on);
            (NOTIFICATIONS, "set-quiet")
        }
        // The clipboard history.
        QsEffect::PasteCopy { copy } => {
            put(&mut arguments, "copy", copy);
            (CLIPBOARD, "paste")
        }
        QsEffect::PinCopy { copy, pinned } => {
            put(&mut arguments, "copy", copy);
            put(&mut arguments, "pinned", pinned);
            (CLIPBOARD, "pin")
        }
        QsEffect::RemoveCopy { copy } => {
            put(&mut arguments, "copy", copy);
            (CLIPBOARD, "remove")
        }
        QsEffect::ClearClipboard => (CLIPBOARD, "clear"),
        // The person's devices.
        QsEffect::PressDevice { row, part } => {
            put(&mut arguments, "row", row);
            arguments.insert(
                "part".into(),
                serde_json::to_value(part).unwrap_or(Value::Null),
            );
            (DEVICES, "press")
        }
        QsEffect::HandOffBluetooth { address, device } => {
            put(&mut arguments, "address", address);
            put(&mut arguments, "device", device);
            (DEVICES, "hand-off")
        }
        // The session itself.
        QsEffect::LockScreen => (SCREEN, "lock"),
        QsEffect::PowerMenu => (SCREEN, "power-menu"),
        QsEffect::SetRotationLock { locked } => {
            put(&mut arguments, "locked", locked);
            (DISPLAY_SETTINGS, "set-rotation-lock")
        }
        QsEffect::SetNightLight { on } => {
            put(&mut arguments, "on", on);
            (NIGHT_LIGHT, "toggle")
        }
        QsEffect::SetPowerMode { mode } => {
            put(&mut arguments, "profile", mode.published());
            (SLEEP, "set-profile")
        }
        QsEffect::OpenSettings { link } => {
            // Settings opens from the desk's search, on the page the link
            // names: its row is the one the person presses there.
            let query = link
                .strip_prefix("settings://")
                .unwrap_or(link)
                .replace(['_', '-'], " ");
            put(&mut arguments, "query", &query);
            (DESK, "search")
        }
        _ => return None,
    };
    Some((resource, action, arguments))
}

/// Write `argument` of `value` into the action's arguments.
fn put<V: serde::Serialize + ?Sized>(
    arguments: &mut Map<String, Value>,
    argument: &str,
    value: &V,
) {
    if let Ok(value) = serde_json::to_value(value) {
        arguments.insert(argument.to_owned(), value);
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use crate::RowPart;

    /// What `effect` is, as the resource, its action and its arguments.
    fn verb(effect: &QsEffect) -> Option<(&'static str, &'static str, Map<String, Value>)> {
        of(&QuickSettingsState::fixture(), effect)
    }

    /// The verb as `resource action key=value ...`, its arguments in name
    /// order.
    fn said(effect: &QsEffect) -> Option<String> {
        let (resource, action, arguments) = verb(effect)?;
        let mut said = format!("{resource} {action}");
        for (name, value) in &arguments {
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            write!(said, " {name}={value}").ok()?;
        }
        Some(said)
    }

    #[test]
    fn every_device_effect_is_one_action_of_the_resource_that_owns_it() {
        assert_eq!(
            said(&QsEffect::ToggleWifi).as_deref(),
            Some("network toggle-wifi"),
            "the radio is the connectivity service's"
        );
        assert_eq!(
            said(&QsEffect::SetBluetooth { on: true }).as_deref(),
            Some("bluetooth set-bluetooth on=true")
        );
        assert_eq!(
            said(&QsEffect::SetVolume { level: 0.5 }).as_deref(),
            Some("sound set-volume level=0.5")
        );
        assert_eq!(
            said(&QsEffect::SetBrightness { level: 0.25 }).as_deref(),
            Some("session:displays set-brightness level=0.25")
        );
        assert_eq!(
            said(&QsEffect::PinCopy {
                copy: "abc".into(),
                pinned: true,
            })
            .as_deref(),
            Some("session:clipboard pin copy=abc pinned=true")
        );
        assert_eq!(
            said(&QsEffect::LockScreen).as_deref(),
            Some("session:screen lock"),
            "the session itself locks"
        );
        assert_eq!(
            said(&QsEffect::SetRotationLock { locked: true }).as_deref(),
            Some("settings:settings.display set-rotation-lock locked=true"),
            "the lock is the person's display setting, kept by the settings service"
        );
    }

    #[test]
    fn a_devices_row_press_is_the_sessions_press_verb() {
        assert_eq!(
            said(&QsEffect::PressDevice {
                row: "laptop".into(),
                part: RowPart::Decline,
            })
            .as_deref(),
            Some("session:devices press row=laptop part=decline"),
            "the part the person pressed goes with the row"
        );
    }

    #[test]
    fn an_accessory_move_is_the_sessions_own() {
        assert_eq!(
            said(&QsEffect::HandOffBluetooth {
                address: "00:1B:66:A1:B2:C3".into(),
                device: "desk".into(),
            })
            .as_deref(),
            Some("session:devices hand-off address=00:1B:66:A1:B2:C3 device=desk"),
            "the session moves the accessory between the person's devices"
        );
    }

    #[test]
    fn the_notifications_row_opens_the_desks_own_centre() {
        assert_eq!(
            said(&QsEffect::ShowCentre).as_deref(),
            Some("desktop:chrome notifications open=true"),
            "one list, in the desk's own centre"
        );
    }

    #[test]
    fn settings_opens_from_the_desks_search_on_the_page_it_names() {
        assert_eq!(
            said(&QsEffect::OpenSettings {
                link: "settings://devices_sync".into(),
            })
            .as_deref(),
            Some("desktop:desk search query=devices sync"),
            "a link is searched for, in the words a person would use"
        );
    }

    #[test]
    fn what_the_panel_carries_out_itself_reaches_no_resource() {
        assert!(
            verb(&QsEffect::AskWifiPassword {
                network: "Home".into(),
            })
            .is_none(),
            "the password prompt is the panel's own"
        );
        assert!(
            verb(&QsEffect::ClearClipboard) == Some((CLIPBOARD, "clear", Map::new())),
            "while clearing the history is the clipboard's"
        );
    }
}
