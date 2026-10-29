//! What Quick Settings does to the device: every switch, slider and row of
//! the panel, published as the panel's effects. A press on a tile, the
//! palette, a script and an agent on this desktop all reach the same effect,
//! and the host that lent the panel its readings carries each one out
//! ([`crate::host::PanelHost::run`]).
//!
//! Every effect changes this device only (`own_device`): its radios, its
//! levels, its clipboard and its notifications.

use ui::tilo_resource::{self, Effect};

/// The panel's effects. The host answers each one; a failure comes back to
/// the caller as the effect's error and the host says so where the person
/// sees it.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.quick-settings.device")]
pub enum QsEffect {
    /// Turn Wi-Fi on or off.
    #[effect(own_device, idempotent, keywords = "wireless, network, radio")]
    SetWifi {
        #[param("Whether Wi-Fi is on")]
        on: bool,
    },
    /// Connect or disconnect the wired network.
    #[effect(own_device, idempotent, keywords = "ethernet, cable")]
    SetWired {
        #[param("Whether the wired network is connected")]
        on: bool,
    },
    /// Turn Bluetooth on or off.
    #[effect(own_device, idempotent)]
    SetBluetooth {
        #[param("Whether Bluetooth is on")]
        on: bool,
    },
    /// Turn airplane mode on or off.
    #[effect(own_device, idempotent, keywords = "flight, radios")]
    SetAirplaneMode {
        #[param("Whether airplane mode is on")]
        on: bool,
    },
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
    #[effect(own_device, idempotent, keywords = "notifications, quiet, focus")]
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
        keywords = "power saver, balanced, performance, battery"
    )]
    SetPowerMode {
        #[param("The power mode")]
        mode: crate::PowerProfile,
    },
    /// Turn a VPN on or off.
    #[effect(own_device, idempotent)]
    SetVpn {
        #[param("The VPN profile, by name")]
        profile: String,
        #[param("Whether it is connected")]
        on: bool,
    },
    /// Set the screen brightness.
    #[effect(own_device, idempotent, keywords = "display, backlight, dim")]
    SetBrightness {
        #[param("Brightness from 0 to 1")]
        level: f64,
    },
    /// Set one display's brightness.
    #[effect(own_device, idempotent, keywords = "monitor, backlight, dim")]
    SetDisplayBrightness {
        #[param("The display, by connector (such as `DP-2`)")]
        display: String,
        #[param("Brightness from 0 to 1")]
        level: f64,
    },
    /// Set the volume.
    #[effect(own_device, idempotent, keywords = "sound, loudness")]
    SetVolume {
        #[param("Volume from 0 to 1")]
        level: f64,
    },
    /// Mute or unmute the sound.
    #[effect(own_device, idempotent, keywords = "sound, silence")]
    SetMuted {
        #[param("Whether the sound is muted")]
        muted: bool,
    },
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
}
