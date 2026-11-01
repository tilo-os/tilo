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
    /// Join a Wi-Fi network that does not broadcast its name.
    #[effect(own_device, icon = "wifi", keywords = "hidden network, hidden ssid")]
    ConnectHiddenWifi {
        #[param("The network's name")]
        network: String,
        #[param(secret, "The network's password; nothing for an open network")]
        password: Option<String>,
    },
    /// Join an enterprise (802.1X) Wi-Fi network with the identity and the
    /// organization's certificates, and connect.
    #[effect(
        own_device,
        icon = "wifi",
        keywords = "802.1x, eap, enterprise, peap, ttls, tls, university, work"
    )]
    ConnectEnterpriseWifi {
        #[param("The profile's name")]
        profile: Option<String>,
        #[param("The network's name (SSID)")]
        ssid: Option<String>,
        #[param("The identity the network knows you by")]
        identity: Option<String>,
        #[param("The identity to send before the real one")]
        anonymous_identity: Option<String>,
        #[param("The organization's CA certificate path")]
        ca_certificate: Option<String>,
        #[param("PEAP, TTLS or TLS")]
        method: Option<String>,
        #[param("MSCHAPv2, PAP or CHAP")]
        inner_auth: Option<String>,
        #[param("The client certificate path")]
        client_certificate: Option<String>,
        #[param("The client key path")]
        private_key: Option<String>,
        #[param(secret, "The password")]
        password: Option<String>,
    },
    /// Leave the Wi-Fi network this device is connected to.
    #[effect(own_device, row, icon = "wifi-off")]
    DisconnectWifi {
        #[param("The network's name")]
        network: String,
    },
    /// Forget a saved network: this device no longer joins it by itself.
    #[effect(own_device, destructive, icon = "trash-2")]
    ForgetNetwork {
        #[param("The saved network, by its id")]
        profile: String,
    },
    /// Join a saved network by itself when it is in reach, or stop.
    #[effect(own_device, idempotent, icon = "wifi")]
    SetNetworkAutoconnect {
        #[param("The saved network, by its id")]
        profile: String,
        #[param("Whether it joins by itself")]
        on: bool,
    },
    /// Send a saved network's traffic through a proxy, or straight out.
    #[effect(own_device, idempotent, icon = "globe")]
    SetNetworkProxy {
        #[param("The saved network, by its id")]
        profile: String,
        #[param("The proxy server as host:port; nothing for no proxy")]
        server: Option<String>,
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
    /// Type the access point name mobile data connects through.
    #[effect(
        own_device,
        idempotent,
        icon = "signal",
        keywords = "apn, access point, operator"
    )]
    SetMobileApn {
        #[param("The access point name; nothing to choose it from the SIM's operator")]
        apn: Option<String>,
    },
    /// Let mobile data connect on another operator's network, or keep it to
    /// this one.
    #[effect(own_device, idempotent, icon = "signal", keywords = "roaming, abroad")]
    SetMobileRoaming {
        #[param("Whether it may roam")]
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
    /// Look for accessories in reach, or stop looking.
    #[effect(
        own_device,
        idempotent,
        icon = "search",
        keywords = "discover, find accessory, pair"
    )]
    Scan {
        #[param("Whether to look and show what is in reach")]
        on: bool,
    },
    /// Pair a device in reach: `BlueZ` bonds with it, trusting it and
    /// connecting it. What the device asks to compare is a question the
    /// person answers.
    #[effect(own_device, icon = "bluetooth", keywords = "pair, connect new")]
    Pair {
        #[param("The device's address")]
        address: String,
    },
    /// Forget a device: its bond, its keys and its trust go, so it must be
    /// paired again from scratch.
    #[effect(own_device, destructive, icon = "trash-2")]
    Forget {
        #[param("The device's address")]
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
    /// Add a VPN from a configuration file's text: `WireGuard`, `OpenVPN` or
    /// a `NetworkManager` key file.
    #[effect(
        own_device,
        icon = "shield",
        keywords = "vpn, import, wireguard, openvpn"
    )]
    ImportVpn {
        #[param("The kind of configuration: wireguard, openvpn, openconnect or a key file")]
        kind: String,
        #[param("What the profile is called")]
        name: String,
        #[param(secret, "The configuration's text")]
        text: String,
    },
    /// Remove a VPN profile from this device.
    #[effect(own_device, destructive, icon = "trash-2")]
    ForgetVpn {
        #[param("The profile, by its id")]
        profile: String,
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
    /// Name the hotspot, choose its band and set its password, then share
    /// the connection over it.
    #[effect(
        own_device,
        icon = "radio-tower",
        keywords = "hotspot name, hotspot password, band"
    )]
    ConfigureHotspot {
        #[param("The hotspot's network name")]
        name: String,
        #[param(secret, "The hotspot's password, at least eight characters")]
        password: String,
        #[param("The band: automatic, two-point-four or five")]
        band: String,
    },
}
