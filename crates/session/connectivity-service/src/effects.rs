//! The connectivity resources' actions. Every one changes this device
//! only (`own_device`).

use tilo_resource::Effect;

/// The `network` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.network")]
pub enum NetworkAction {
    /// Turn Wi-Fi on or off.
    #[effect(
        own_device,
        idempotent,
        icon = "wifi",
        keywords = "wireless, radio, wlan, wifi on, wifi off"
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
        keywords = "wifi, wi-fi, wlan, wireless"
    )]
    ToggleWifi,
    /// Look again for Wi-Fi networks in reach.
    #[effect(
        own_device,
        idempotent,
        icon = "refresh-cw",
        keywords = "scan, search networks"
    )]
    ScanWifi,
    /// Join a Wi-Fi network: one this device knows, or an open one.
    #[effect(own_device, row, icon = "wifi")]
    JoinWifi {
        #[param("The network's name")]
        network: String,
    },
    /// Join a protected Wi-Fi network with its password.
    #[effect(own_device, icon = "wifi")]
    ConnectWifi {
        #[param("The network's name")]
        network: String,
        #[param(secret, "The network's password")]
        password: String,
    },
    /// Leave the Wi-Fi network this device is connected to.
    #[effect(own_device, row, icon = "wifi-off")]
    DisconnectWifi {
        #[param("The network's name")]
        network: String,
    },
    /// Connect or disconnect the wired network.
    #[effect(
        own_device,
        idempotent,
        icon = "network",
        keywords = "ethernet, cable, lan"
    )]
    SetWired {
        #[param("Whether the wired network is connected")]
        on: bool,
    },
    /// Turn mobile data on or off.
    #[effect(
        own_device,
        idempotent,
        icon = "signal",
        keywords = "cellular, broadband, lte, 5g"
    )]
    SetMobileData {
        #[param("Whether mobile data is on")]
        on: bool,
    },
    /// Turn airplane mode on or off: every wireless radio goes off with it.
    #[effect(
        own_device,
        idempotent,
        icon = "plane",
        keywords = "flight mode, radios off, offline, flugmodus"
    )]
    SetAirplaneMode {
        #[param("Whether airplane mode is on")]
        on: bool,
    },
    /// Turn airplane mode off when it is on, and on when it is off.
    #[effect(
        own_device,
        label = "Airplane mode",
        icon = "plane",
        keywords = "airplane mode, flight mode"
    )]
    ToggleAirplaneMode,
}

/// The `bluetooth` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.bluetooth")]
pub enum BluetoothAction {
    /// Turn Bluetooth on or off.
    #[effect(
        own_device,
        idempotent,
        icon = "bluetooth",
        keywords = "bluetooth on, bluetooth off"
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
        keywords = "bluetooth, headphones"
    )]
    ToggleBluetooth,
    /// Connect a paired accessory.
    #[effect(own_device, row, icon = "bluetooth")]
    Connect {
        #[param("The accessory's address")]
        address: String,
    },
    /// Disconnect an accessory.
    #[effect(own_device, row, icon = "bluetooth-off")]
    Disconnect {
        #[param("The accessory's address")]
        address: String,
    },
}

/// The `vpn` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.vpn")]
pub enum VpnAction {
    /// Connect or disconnect a VPN profile.
    #[effect(
        own_device,
        idempotent,
        icon = "shield",
        keywords = "vpn, tunnel, wireguard"
    )]
    SetVpn {
        #[param("The profile, by its id")]
        profile: String,
        #[param("Whether it is connected")]
        on: bool,
    },
}

/// The `tethering` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.tethering")]
pub enum TetheringAction {
    /// Share this device's connection over a Wi-Fi hotspot, or stop.
    #[effect(
        own_device,
        idempotent,
        icon = "radio-tower",
        keywords = "hotspot, tether, share connection"
    )]
    SetHotspot {
        #[param("Whether the hotspot is on")]
        on: bool,
    },
}
