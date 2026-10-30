//! What Quick Settings does to the device: every switch, slider and row of
//! the panel, published as the panel's effects. A press on a tile, the
//! palette, a script and an agent on this desktop all reach the same effect,
//! and the host that lent the panel its readings carries each one out
//! ([`crate::host::PanelHost::run`]).
//!
//! Every effect changes this device only (`own_device`): its radios, its
//! levels, its clipboard and its notifications.

use ui::framework::EffectStatus;
use ui::tilo_resource::{self, ActionRef, Effect};

use crate::i18n::t;

/// The panel's effects. The host answers each one; a failure comes back to
/// the caller as the effect's error and the host says so where the person
/// sees it.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.quick-settings.device")]
pub enum QsEffect {
    /// Turn Wi-Fi on or off.
    #[effect(
        own_device,
        idempotent,
        icon = "wifi",
        keywords = "wireless, radio, wifi on, wifi off, enable wifi, disable wifi, wlan an, wlan aus, wlan einschalten, wlan ausschalten"
    )]
    SetWifi {
        #[param("Whether Wi-Fi is on")]
        on: bool,
    },
    /// Turn Wi-Fi off when it is on, and on when it is off.
    #[effect(
        own_device,
        label = "Wi-Fi",
        icon = "wifi",
        keywords = "wifi, wi-fi, wlan, wireless, internet, network, wireless network, drahtlos, netzwerk, funknetz"
    )]
    ToggleWifi,
    /// Connect or disconnect the wired network.
    #[effect(own_device, idempotent, keywords = "ethernet, cable")]
    SetWired {
        #[param("Whether the wired network is connected")]
        on: bool,
    },
    /// Turn Bluetooth on or off.
    #[effect(
        own_device,
        idempotent,
        icon = "bluetooth",
        keywords = "bluetooth on, bluetooth off, enable bluetooth, disable bluetooth, bluetooth an, bluetooth aus, bluetooth einschalten, bluetooth ausschalten"
    )]
    SetBluetooth {
        #[param("Whether Bluetooth is on")]
        on: bool,
    },
    /// Turn Bluetooth off when it is on, and on when it is off.
    #[effect(
        own_device,
        label = "Bluetooth",
        icon = "bluetooth",
        keywords = "bluetooth, headphones, pair, kopfhörer, koppeln"
    )]
    ToggleBluetooth,
    /// Turn airplane mode on or off.
    #[effect(
        own_device,
        idempotent,
        icon = "plane",
        keywords = "flight, radios, airplane mode on, airplane mode off, flight mode on, flight mode off, flugmodus an, flugmodus aus"
    )]
    SetAirplaneMode {
        #[param("Whether airplane mode is on")]
        on: bool,
    },
    /// Turn airplane mode off when it is on, and on when it is off: every
    /// wireless radio goes off with it.
    #[effect(
        own_device,
        label = "Airplane mode",
        icon = "plane",
        keywords = "airplane mode, flight mode, aeroplane, radios off, offline, flugmodus, flugzeugmodus, funk aus"
    )]
    ToggleAirplaneMode,
    /// Turn mobile data on or off.
    #[effect(own_device, idempotent, keywords = "cellular, broadband")]
    SetMobileData {
        #[param("Whether mobile data is on")]
        on: bool,
    },
    /// Turn night light on or off.
    #[effect(own_device, idempotent, keywords = "warm colours, blue light")]
    SetNightLight {
        #[param("Whether night light is on")]
        on: bool,
    },
    /// Turn Do Not Disturb on or off.
    #[effect(own_device, idempotent, keywords = "quiet, focus")]
    SetDoNotDisturb {
        #[param("Whether notifications are held")]
        on: bool,
    },
    /// Lock or unlock the screen's rotation.
    #[effect(own_device, idempotent, keywords = "orientation")]
    SetRotationLock {
        #[param("Whether the screen stays put when the device turns")]
        locked: bool,
    },
    /// Choose the power mode.
    #[effect(
        own_device,
        idempotent,
        label = "Power mode",
        icon = "gauge",
        keywords = "power saver, balanced, performance, power profile, battery saver, performance mode, power saving, energy, energieprofil, energiesparmodus, akkusparmodus, leistungsmodus, energiesparen"
    )]
    SetPowerMode {
        #[param("The power mode")]
        mode: crate::PowerProfile,
    },
    /// Turn a VPN on or off.
    #[effect(
        own_device,
        idempotent,
        icon = "shield",
        keywords = "vpn, tunnel, wireguard, proxy, verbindung"
    )]
    SetVpn {
        #[param("The VPN profile, by name")]
        profile: String,
        #[param("Whether it is connected")]
        on: bool,
    },
    /// Set the screen brightness.
    #[effect(
        own_device,
        idempotent,
        label = "Brightness",
        icon = "sun",
        keywords = "display, backlight, dim, screen brightness, display brightness, helligkeit, bildschirmhelligkeit, hintergrundbeleuchtung"
    )]
    SetBrightness {
        #[param("Brightness from 0 to 1")]
        level: f64,
    },
    /// Make the screen brighter by a step: the display the person works on,
    /// when there are several.
    #[effect(
        own_device,
        label = "Brighter",
        icon = "sun",
        keywords = "brighter, brightness up, heller"
    )]
    BrightnessUp,
    /// Make the screen dimmer by a step: the display the person works on,
    /// when there are several.
    #[effect(
        own_device,
        label = "Dimmer",
        icon = "sun",
        keywords = "dimmer, darker, brightness down, dunkler"
    )]
    BrightnessDown,
    /// Set one display's brightness.
    #[effect(own_device, idempotent, keywords = "monitor, backlight, dim")]
    SetDisplayBrightness {
        #[param("The display, by connector (such as `DP-2`)")]
        display: String,
        #[param("Brightness from 0 to 1")]
        level: f64,
    },
    /// Set the volume.
    #[effect(
        own_device,
        idempotent,
        label = "Volume",
        icon = "volume-2",
        keywords = "sound, loudness, audio level, speaker, lautstärke, lautstaerke, ton, audio"
    )]
    SetVolume {
        #[param("Volume from 0 to 1")]
        level: f64,
    },
    /// Turn the volume up by a step.
    #[effect(
        own_device,
        label = "Louder",
        icon = "volume-2",
        keywords = "louder, volume up, turn up, lauter, lauter machen"
    )]
    VolumeUp,
    /// Turn the volume down by a step.
    #[effect(
        own_device,
        label = "Quieter",
        icon = "volume-2",
        keywords = "quieter, softer, volume down, turn down, leiser, leiser machen"
    )]
    VolumeDown,
    /// Mute or unmute the sound.
    #[effect(own_device, idempotent, icon = "volume-x", keywords = "sound, silence")]
    SetMuted {
        #[param("Whether the sound is muted")]
        muted: bool,
    },
    /// Unmute the sound when it is muted, and mute it when it is not.
    #[effect(
        own_device,
        label = "Mute or unmute",
        icon = "volume-x",
        keywords = "mute, unmute, silence, stumm, stummschalten, ton aus"
    )]
    ToggleMuted,
    /// Play sound through an output.
    #[effect(own_device, idempotent, keywords = "speakers, headphones, audio")]
    RouteSound {
        #[param("The output's audio node")]
        node: u32,
    },
    /// Join a Wi-Fi network that needs no password typed.
    #[effect(own_device)]
    JoinWifi {
        #[param("The network's name")]
        network: String,
    },
    /// Ask for a protected Wi-Fi network's password.
    #[effect(own_device, audience = "people")]
    AskWifiPassword {
        #[param("The network's name")]
        network: String,
    },
    /// Join a protected Wi-Fi network with its password.
    #[effect(own_device)]
    ConnectWifi {
        #[param("The network's name")]
        network: String,
        #[param(secret, "The network's password")]
        password: String,
    },
    /// Stop asking for a Wi-Fi password.
    #[effect(own_device, audience = "people")]
    CancelWifiPassword,
    /// Ask for the name and password of a network that broadcasts no name.
    #[effect(own_device, audience = "people", icon = "wifi")]
    AskHiddenNetwork,
    /// Stop asking for a hidden network's name and password.
    #[effect(own_device, audience = "people")]
    CancelHiddenNetwork,
    /// Join a network that broadcasts no name, by the name and password
    /// typed for it.
    #[effect(own_device)]
    ConnectHiddenWifi {
        #[param("The network's name")]
        network: String,
        #[param(secret, "The network's password")]
        password: String,
    },
    /// Join a saved network by itself whenever it is in reach, or wait to be
    /// asked.
    #[effect(own_device, idempotent, icon = "wifi", keywords = "saved network")]
    SetNetworkAutoconnect {
        #[param("The saved network")]
        profile: String,
        #[param("Whether it joins by itself")]
        on: bool,
    },
    /// Forget a saved network, so this machine joins it only when asked and
    /// its password must be typed again: the profile and the password it
    /// keeps go.
    #[effect(
        own_device,
        destructive,
        icon = "trash-2",
        keywords = "saved network, forget, remove"
    )]
    ForgetNetwork {
        #[param("The saved network")]
        profile: String,
    },
    /// Connect a paired Bluetooth accessory.
    #[effect(own_device)]
    ConnectBluetooth {
        #[param("The accessory's address")]
        address: String,
    },
    /// Disconnect a Bluetooth accessory.
    #[effect(own_device)]
    DisconnectBluetooth {
        #[param("The accessory's address")]
        address: String,
    },
    /// Look for Bluetooth devices in reach, or stop looking.
    #[effect(
        own_device,
        idempotent,
        icon = "search",
        keywords = "discover, find device, pair, new accessory"
    )]
    ScanBluetooth {
        #[param("Whether to look for devices")]
        on: bool,
    },
    /// Pair the Bluetooth device at this address: `BlueZ` bonds with it,
    /// trusting it and connecting it. What it asks to compare is a question
    /// its pairing agent asks the person.
    #[effect(own_device, icon = "bluetooth", keywords = "pair, bond, new accessory")]
    PairBluetooth {
        #[param("The device's address")]
        address: String,
    },
    /// Forget a Bluetooth device, so that it must be paired again from
    /// scratch: its bond, its keys and its trust go.
    #[effect(own_device, destructive, icon = "trash-2", keywords = "unpair, remove")]
    ForgetBluetooth {
        #[param("The device's address")]
        address: String,
    },
    /// Move a Bluetooth accessory to another of your devices.
    #[effect(own_device)]
    HandOffBluetooth {
        #[param("The accessory's address")]
        address: String,
        #[param("The device to move it to")]
        device: String,
    },
    /// Act on a row of the Devices section.
    #[effect(own_device)]
    PressDevice {
        #[param("The row")]
        row: String,
        #[param("What on the row was pressed")]
        part: crate::RowPart,
    },
    /// Paste a copy from the clipboard history.
    #[effect(own_device, keywords = "clipboard history")]
    PasteCopy {
        #[param("The copy")]
        copy: String,
    },
    /// Pin or unpin a copy in the clipboard history.
    #[effect(own_device, idempotent)]
    PinCopy {
        #[param("The copy")]
        copy: String,
        #[param("Whether it is kept when the history is cleared")]
        pinned: bool,
    },
    /// Remove a copy from the clipboard history. It goes at a press, as a
    /// notification is dismissed: the history is a convenience, not a
    /// record anyone keeps.
    #[effect(own_device)]
    RemoveCopy {
        #[param("The copy")]
        copy: String,
    },
    /// Clear the clipboard history, keeping pinned copies.
    #[effect(own_device)]
    ClearClipboard,
    /// Open a notification.
    #[effect(own_device)]
    OpenNotification {
        #[param("The notification")]
        notification: u64,
    },
    /// Dismiss a notification.
    #[effect(own_device)]
    DismissNotification {
        #[param("The notification")]
        notification: u64,
    },
    /// Dismiss every notification from one app.
    #[effect(own_device)]
    DismissNotificationGroup {
        #[param("The app's notification group")]
        group: String,
    },
    /// Run one of a notification's actions.
    #[effect(own_device)]
    RunNotificationAction {
        #[param("The notification")]
        notification: u64,
        #[param("The action")]
        action: String,
    },
    /// Hand a notification to an agent as a task.
    #[effect(own_device)]
    HandNotificationToAgent {
        #[param("The notification")]
        notification: u64,
    },
    /// Clear all notifications.
    #[effect(own_device)]
    ClearNotifications,
    /// Bring back the notifications just cleared.
    #[effect(own_device)]
    UndoClearNotifications,
    /// Show a control on the panel.
    #[effect(own_device, idempotent)]
    ShowControl {
        #[param(
            "The control: `wifi`, `bluetooth`, `vpn`, `night_light`, `airplane_mode`, `mobile_data`, `do_not_disturb`, `rotation_lock` or `power_mode`"
        )]
        control: String,
    },
    /// Take a control off the panel.
    #[effect(own_device, idempotent)]
    HideControl {
        #[param("The control, as Show control names it")]
        control: String,
    },
    /// Move a control to another place on the panel.
    #[effect(own_device, idempotent)]
    MoveControl {
        #[param("The control, as Show control names it")]
        control: String,
        #[param("Its new place among the controls shown, from 0")]
        to: u32,
    },
    /// Put the panel's controls back as they were at first.
    #[effect(own_device)]
    ResetControls,
    /// Open Settings, at a page when one is named.
    #[effect(own_device, keywords = "preferences")]
    OpenSettings {
        #[param("A Settings link, such as `settings://network`; empty for Settings itself")]
        link: String,
    },
    /// Lock the screen.
    #[effect(own_device)]
    LockScreen,
    /// Show the power menu: sleep, restart, shut down.
    #[effect(own_device, audience = "people")]
    PowerMenu,
    /// Play or pause what plays.
    #[effect(own_device, keywords = "music, media, resume, pause music")]
    PlayPause,
    /// Go back to the previous track.
    #[effect(own_device, keywords = "music, media, back, rewind")]
    PreviousTrack,
    /// Skip to the next track.
    #[effect(own_device, keywords = "music, media, skip")]
    NextTrack,
    /// Go to a point in what plays.
    #[effect(own_device, idempotent)]
    Seek {
        #[param("How far into the song, from 0 to 1")]
        fraction: f64,
    },
    /// Steer another of the running players.
    #[effect(own_device, idempotent)]
    ChoosePlayer {
        #[param("The player, as the now-playing card lists it")]
        player: String,
    },
    /// Play the sound somewhere else: this computer, one of the person's
    /// devices or speaker groups.
    #[effect(own_device, keywords = "cast, speaker, airplay, output")]
    ChooseOutput {
        #[param("The destination, as the now-playing card lists it")]
        output: String,
    },
}

/// What callers are told when the device lacks the control an effect
/// works.
const LACKS_CONTROL: &str = "quick_settings_unavailable";
/// What callers are told while a radio change is under way.
const RADIO_PENDING: &str = "quick_settings_radio_pending";
/// What callers are told while nothing plays that offers the control.
const NOTHING_PLAYS: &str = "quick_settings_nothing_plays";

/// The effects as callers see them now, from the device's readings: one
/// whose control the device lacks, by the desktop's probe and by what it
/// last read, is unavailable and says so; a radio waits for the change
/// already under way; and the effects search offers a person are named in
/// the desktop's language.
pub(crate) fn statuses(state: &crate::QuickSettingsState) -> Vec<EffectStatus> {
    let probed = state.available;
    let radios = &state.radios;
    let can = crate::actions::QsActionAvailability {
        brightness: state.can_set_brightness(),
        volume: probed.volume || state.volume_available,
        wifi: probed.wifi || radios.wifi_available,
        power_profile: probed.power_profile || !state.power_profiles.is_empty(),
        vpn: probed.vpn || !state.vpns.is_empty(),
        bluetooth: probed.bluetooth || state.bluetooth_available,
        airplane_mode: probed.airplane_mode || radios.wifi_available || radios.wwan_available,
    };
    let mut statuses = radio_statuses(can, state.radio_change_pending);
    statuses.extend(level_statuses(can));
    statuses.extend(media_statuses(state.now_playing.as_ref()));
    statuses
}

/// What plays: each control is there while the player in front offers it.
fn media_statuses(playing: Option<&crate::NowPlaying>) -> Vec<EffectStatus> {
    let idle = || (NOTHING_PLAYS, t("qs-nothing-plays"));
    let offered = |present: bool| (!present).then(idle);
    let np = playing.cloned().unwrap_or_default();
    let some = playing.is_some();
    [
        shown(QsEffect::PLAY_PAUSE, None, offered(some && np.can_toggle)),
        shown(
            QsEffect::PREVIOUS_TRACK,
            None,
            offered(some && np.can_previous),
        ),
        shown(QsEffect::NEXT_TRACK, None, offered(some && np.can_next)),
        shown(QsEffect::SEEK, None, offered(some && np.seekable())),
        shown(QsEffect::CHOOSE_PLAYER, None, offered(np.players.len() > 1)),
        shown(
            QsEffect::CHOOSE_OUTPUT,
            None,
            offered(!np.outputs.is_empty()),
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Why an effect cannot be used, when the device lacks its control.
fn lacks(present: bool) -> Option<(&'static str, String)> {
    (!present).then(|| (LACKS_CONTROL, t("qs-unavailable")))
}

/// The radios' effects: Wi-Fi, airplane mode and Bluetooth.
fn radio_statuses(can: crate::actions::QsActionAvailability, pending: bool) -> Vec<EffectStatus> {
    let radio = |present: bool| {
        lacks(present).or_else(|| pending.then(|| (RADIO_PENDING, t("display-confirm-applying"))))
    };
    [
        shown(
            QsEffect::SET_WIFI,
            Some(t("qs-action-set-wifi")),
            radio(can.wifi),
        ),
        shown(QsEffect::TOGGLE_WIFI, Some(t("qs-wifi")), radio(can.wifi)),
        shown(QsEffect::JOIN_WIFI, None, lacks(can.wifi)),
        shown(QsEffect::ASK_WIFI_PASSWORD, None, lacks(can.wifi)),
        shown(QsEffect::CONNECT_WIFI, None, lacks(can.wifi)),
        shown(QsEffect::ASK_HIDDEN_NETWORK, None, lacks(can.wifi)),
        shown(QsEffect::CANCEL_HIDDEN_NETWORK, None, lacks(can.wifi)),
        shown(QsEffect::CONNECT_HIDDEN_WIFI, None, lacks(can.wifi)),
        shown(QsEffect::SET_NETWORK_AUTOCONNECT, None, lacks(can.wifi)),
        shown(QsEffect::FORGET_NETWORK, None, lacks(can.wifi)),
        shown(
            QsEffect::SET_AIRPLANE_MODE,
            Some(t("qs-action-set-airplane-mode")),
            radio(can.airplane_mode),
        ),
        shown(
            QsEffect::TOGGLE_AIRPLANE_MODE,
            Some(t("qs-airplane-mode")),
            radio(can.airplane_mode),
        ),
        shown(
            QsEffect::SET_BLUETOOTH,
            Some(t("qs-action-set-bluetooth")),
            lacks(can.bluetooth),
        ),
        shown(
            QsEffect::TOGGLE_BLUETOOTH,
            Some(t("qs-bluetooth")),
            lacks(can.bluetooth),
        ),
        shown(QsEffect::CONNECT_BLUETOOTH, None, lacks(can.bluetooth)),
        shown(QsEffect::DISCONNECT_BLUETOOTH, None, lacks(can.bluetooth)),
        shown(QsEffect::SCAN_BLUETOOTH, None, lacks(can.bluetooth)),
        shown(QsEffect::PAIR_BLUETOOTH, None, lacks(can.bluetooth)),
        shown(QsEffect::FORGET_BLUETOOTH, None, lacks(can.bluetooth)),
        shown(QsEffect::HAND_OFF_BLUETOOTH, None, lacks(can.bluetooth)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The levels' effects, the power mode's and the VPNs'.
fn level_statuses(can: crate::actions::QsActionAvailability) -> Vec<EffectStatus> {
    [
        shown(
            QsEffect::SET_BRIGHTNESS,
            Some(t("qs-brightness")),
            lacks(can.brightness),
        ),
        shown(
            QsEffect::SET_DISPLAY_BRIGHTNESS,
            None,
            lacks(can.brightness),
        ),
        shown(
            QsEffect::BRIGHTNESS_UP,
            Some(t("qs-action-brighter")),
            lacks(can.brightness),
        ),
        shown(
            QsEffect::BRIGHTNESS_DOWN,
            Some(t("qs-action-dimmer")),
            lacks(can.brightness),
        ),
        shown(
            QsEffect::SET_VOLUME,
            Some(t("qs-volume")),
            lacks(can.volume),
        ),
        shown(
            QsEffect::VOLUME_UP,
            Some(t("qs-action-louder")),
            lacks(can.volume),
        ),
        shown(
            QsEffect::VOLUME_DOWN,
            Some(t("qs-action-quieter")),
            lacks(can.volume),
        ),
        shown(QsEffect::SET_MUTED, None, lacks(can.volume)),
        shown(
            QsEffect::TOGGLE_MUTED,
            Some(t("qs-action-mute-toggle")),
            lacks(can.volume),
        ),
        shown(
            QsEffect::SET_POWER_MODE,
            Some(t("qs-power-mode")),
            lacks(can.power_profile),
        ),
        shown(
            QsEffect::SET_VPN,
            Some(t("qs-action-set-vpn")),
            lacks(can.vpn),
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// `effect` as `label` names it and, when `unavailable` says why, not to be
/// invoked now; `None` when it is as declared.
fn shown<A>(
    effect: ActionRef<QsEffect, A>,
    label: Option<String>,
    unavailable: Option<(&str, String)>,
) -> Option<EffectStatus> {
    match (unavailable, label) {
        (Some((code, reason)), label) => {
            let status = EffectStatus::unavailable(effect, code, reason);
            Some(match label {
                Some(label) => status.with_label(label),
                None => status,
            })
        }
        (None, Some(label)) => Some(EffectStatus::labelled(effect, label)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A control the device lacks is published unavailable, saying why; a
    /// radio waits while a change of it is under way; the rest are offered
    /// under their names.
    #[test]
    fn effects_follow_what_the_device_can_do() {
        let mut state = crate::QuickSettingsState::fixture();
        state.available = crate::actions::QsActionAvailability {
            brightness: false,
            ..crate::actions::QsActionAvailability::all()
        };
        state.brightness_available = false;
        let offered = statuses(&state);
        assert!(offered.contains(&EffectStatus::labelled(QsEffect::SET_VOLUME, "Volume")));
        assert!(offered.contains(&EffectStatus::labelled(QsEffect::TOGGLE_WIFI, "Wi-Fi")));
        assert!(
            offered.contains(
                &EffectStatus::unavailable(
                    QsEffect::BRIGHTNESS_UP,
                    LACKS_CONTROL,
                    "This device has no such control",
                )
                .with_label("Brighter")
            )
        );
        state.available = crate::actions::QsActionAvailability::all();
        state.radio_change_pending = true;
        assert!(
            statuses(&state).contains(
                &EffectStatus::unavailable(QsEffect::SET_WIFI, RADIO_PENDING, "Applying…")
                    .with_label("Turn Wi-Fi on or off")
            )
        );
    }
}
