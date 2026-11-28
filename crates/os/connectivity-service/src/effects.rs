//! The connectivity resources' actions. Every one changes this device
//! only (`own_device`).

use tilo_resource::Effect;

use crate::{
    BluetoothAccessoryRow, BluetoothNearbyRow, SavedNetworkRow, VpnProfileRow, WifiNetworkRow,
};

/// A Wi-Fi network's own actions, called on the network's address
/// (`wifi-network:Home`) as every caller names it.
#[derive(Clone, Copy, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.wifi-network")]
pub enum WifiNetworkAction {
    /// Join the network: one this device knows, or an open one.
    #[effect(kind = "outside")]
    #[effect(own_device, row, icon = "wifi", on = WifiNetworkRow)]
    Join,
    /// Leave the network, when this device is connected to it.
    #[effect(kind = "outside")]
    #[effect(own_device, row, icon = "wifi-off", on = WifiNetworkRow)]
    Leave,
}

/// A saved network's own actions, called on the saved network's address
/// (`saved-network:<id>`).
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.saved-network")]
pub enum SavedNetworkAction {
    /// Join the saved network now.
    #[effect(kind = "outside")]
    #[effect(own_device, row, icon = "wifi", on = SavedNetworkRow)]
    Join,
    /// Join the network by itself when it is in reach, or stop.
    #[effect(kind = "outside")]
    #[effect(own_device, row, idempotent, icon = "wifi", on = SavedNetworkRow)]
    SetAutoconnect {
        #[param("Whether it joins by itself")]
        on: bool,
    },
    /// Forget the network: this device no longer joins it by itself.
    #[effect(kind = "outside")]
    #[effect(own_device, row, destructive, icon = "trash-2", on = SavedNetworkRow)]
    Forget,
    /// Send the network's traffic through a proxy, or straight out.
    #[effect(kind = "outside")]
    #[effect(own_device, row, idempotent, icon = "globe", on = SavedNetworkRow)]
    SetProxy {
        #[param("The proxy server as host:port; nothing for no proxy")]
        server: Option<String>,
    },
    /// Wake this device from sleep when a wake packet arrives over the network, or stop.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        row,
        idempotent,
        icon = "power",
        keywords = "wake on lan, wake on wlan, magic packet",
        on = SavedNetworkRow
    )]
    SetWake {
        #[param("Whether the network wakes this device")]
        on: bool,
    },
    /// Share the Wi-Fi network's password with your other devices, so they join it too.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        row,
        icon = "share-2",
        keywords = "wifi password, other devices, continuity",
        on = SavedNetworkRow
    )]
    ShareWithDevices,
}

/// The `network` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.network")]
pub enum NetworkAction {
    /// Turn Wi-Fi on or off.
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "wifi", keywords = "wifi, wi-fi, wlan, wireless")]
    ToggleWifi,
    /// Look again for Wi-Fi networks in reach.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "refresh-cw",
        keywords = "scan, search networks"
    )]
    ScanWifi,
    /// Join a protected Wi-Fi network with its password.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "wifi")]
    ConnectWifi {
        #[param("The network's name")]
        network: String,
        #[param(secret, "The network's password")]
        password: String,
    },
    /// Join a Wi-Fi network that does not broadcast its name.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "wifi", keywords = "hidden network, hidden ssid")]
    ConnectHiddenWifi {
        #[param("The network's name")]
        network: String,
        #[param(secret, "The network's password; nothing for an open network")]
        password: Option<String>,
    },
    /// Join an enterprise (802.1X) Wi-Fi network with the identity and the
    /// organization's certificates, and connect.
    #[effect(kind = "outside")]
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
    /// Connect or disconnect the wired network.
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "signal", keywords = "roaming, abroad")]
    SetMobileRoaming {
        #[param("Whether it may roam")]
        on: bool,
    },
    /// Turn airplane mode on or off: every wireless radio goes off with it.
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "plane", keywords = "airplane mode, flight mode")]
    ToggleAirplaneMode,
    /// Count mobile data against a limit, or let the network say.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "signal",
        keywords = "metered, data limit, data usage, datenlimit"
    )]
    SetMobileMetered {
        #[param("automatic, metered or unmetered")]
        metered: String,
    },
    /// Set how a saved network gets its IPv4 address: automatically, or the
    /// addresses, gateway and name servers given.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "network",
        keywords = "ip address, static ip, dhcp, gateway",
        on = SavedNetworkRow
    )]
    SetNetworkIpv4 {
        #[param("The saved network, by its id")]
        profile: String,
        #[param("Whether the network hands out the address")]
        automatic: bool,
        #[param("The addresses with their prefix (192.168.1.20/24), when not automatic")]
        addresses: Vec<String>,
        #[param("The gateway, when not automatic")]
        gateway: Option<String>,
        #[param("Name servers of its own; none takes the network's")]
        dns: Vec<String>,
    },
    /// Set how a saved network gets its IPv6 address.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "network",
        keywords = "ipv6, static ip, slaac",
        on = SavedNetworkRow
    )]
    SetNetworkIpv6 {
        #[param("The saved network, by its id")]
        profile: String,
        #[param("automatic, manual, link-local or disabled")]
        method: String,
        #[param("The addresses with their prefix (2001:db8::10/64), when manual")]
        addresses: Vec<String>,
        #[param("The gateway, when manual")]
        gateway: Option<String>,
        #[param("Name servers of its own; none takes the network's")]
        dns: Vec<String>,
    },
    /// Set the name servers a saved network uses; none takes the network's.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "globe",
        keywords = "dns, name server",
        on = SavedNetworkRow
    )]
    SetNetworkDns {
        #[param("The saved network, by its id")]
        profile: String,
        #[param("The name servers")]
        servers: Vec<String>,
    },
    /// Change how a saved network is used: whether it is metered, the
    /// hardware address this device shows it, its proxy, its priority and
    /// whether its name is hidden. What is left out stays as it is.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "settings",
        keywords = "metered, mac address, proxy, priority, hidden",
        on = SavedNetworkRow
    )]
    SetNetworkPolicy {
        #[param("The saved network, by its id")]
        profile: String,
        #[param("automatic, metered or unmetered")]
        metered: Option<String>,
        #[param("The hardware address it is shown: permanent, random or stable")]
        mac_address: Option<String>,
        #[param("Its proxy: none, automatic or manual")]
        proxy: Option<String>,
        #[param("The proxy's configuration address (automatic) or host:port (manual)")]
        proxy_address: Option<String>,
        #[param("Whether only the browser uses the proxy")]
        browser_only: Option<bool>,
        #[param("Which saved network wins when several are in reach: higher first")]
        priority: Option<i64>,
        #[param("Whether the Wi-Fi network keeps its name hidden")]
        hidden: Option<bool>,
    },
    /// Change who a work or school network knows this device as, and the
    /// certificate it checks the network with.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "id-card",
        keywords = "802.1x, enterprise, identity, certificate",
        on = SavedNetworkRow
    )]
    SetEnterpriseIdentity {
        #[param("The saved network, by its id")]
        profile: String,
        #[param("The identity")]
        identity: String,
        #[param("The CA certificate's path; empty when the network is not checked")]
        ca_certificate: String,
    },
    /// Change the password a saved Wi-Fi network joins with.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "key",
        keywords = "wifi password",
        on = SavedNetworkRow
    )]
    SetWifiPassword {
        #[param("The saved network, by its id")]
        profile: String,
        #[param(secret, "The network's password")]
        password: String,
    },
    /// Ask before joining an open Wi-Fi network this device keeps, or let the ones it keeps join
    /// by themselves.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "wifi",
        keywords = "open network, ask first, join by itself, auto connect"
    )]
    SetAskBeforeOpenNetworks {
        #[param("Whether this device asks before it joins an open network")]
        on: bool,
    },
}

/// The `bluetooth` resource's actions.
#[derive(Clone, Copy, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.bluetooth")]
pub enum BluetoothAction {
    /// Turn Bluetooth on or off.
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "bluetooth", keywords = "bluetooth, headphones")]
    ToggleBluetooth,
    /// Look for accessories in reach, or stop looking.
    #[effect(kind = "outside")]
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
}

/// A paired Bluetooth accessory's own actions, called on its address
/// (`bluetooth-accessory:<hardware address>`).
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.bluetooth-accessory")]
pub enum BluetoothAccessoryAction {
    /// Connect the accessory.
    #[effect(kind = "outside")]
    #[effect(own_device, row, icon = "bluetooth", on = BluetoothAccessoryRow)]
    Connect,
    /// Disconnect the accessory.
    #[effect(kind = "outside")]
    #[effect(own_device, row, icon = "bluetooth-off", on = BluetoothAccessoryRow)]
    Disconnect,
    /// Forget the accessory: its bond, its keys and its trust go, so it must
    /// be paired again from scratch.
    #[effect(kind = "outside")]
    #[effect(own_device, row, destructive, icon = "trash-2", on = BluetoothAccessoryRow)]
    Forget,
    /// Rename the accessory: what this machine calls it, kept by `BlueZ`
    /// beside the name the device reports itself under.
    #[effect(kind = "outside")]
    #[effect(own_device, row, icon = "pencil", on = BluetoothAccessoryRow)]
    Rename {
        #[param("What the accessory is called")]
        name: String,
    },
    /// Let the accessory connect by itself, or ask the person first.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        row,
        idempotent,
        icon = "shield-check",
        keywords = "trust, connect by itself",
        on = BluetoothAccessoryRow
    )]
    SetTrusted {
        #[param("Whether the accessory may connect without asking")]
        trusted: bool,
    },
}

/// A Bluetooth device in reach, called on its address
/// (`bluetooth-nearby:<hardware address>`).
#[derive(Clone, Copy, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.bluetooth-nearby")]
pub enum BluetoothNearbyAction {
    /// Pair the device: `BlueZ` bonds with it, trusting it and connecting
    /// it. What the device asks to compare is a question the person answers.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        row,
        icon = "bluetooth",
        keywords = "pair, connect new",
        on = BluetoothNearbyRow
    )]
    Pair,
}

/// The `vpn` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.vpn")]
pub enum VpnAction {
    /// Add a VPN from a configuration file's text: `WireGuard`, `OpenVPN` or
    /// a `NetworkManager` key file.
    #[effect(kind = "outside")]
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
    /// Route every connection on this device over Tor, or stop.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "globe",
        keywords = "tor, anonymous, onion, privacy, exit node"
    )]
    SetTor {
        #[param("Whether every connection goes over Tor")]
        on: bool,
    },
}

/// A VPN profile's own actions, called on its address
/// (`vpn-profile:<id>`).
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.vpn-profile")]
pub enum VpnProfileAction {
    /// Connect the VPN.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        row,
        icon = "shield",
        keywords = "vpn, tunnel, wireguard",
        on = VpnProfileRow
    )]
    Connect,
    /// Disconnect the VPN.
    #[effect(kind = "outside")]
    #[effect(own_device, row, icon = "shield-off", on = VpnProfileRow)]
    Disconnect,
    /// Change the profile. What is left out stays as it is.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        row,
        idempotent,
        icon = "shield",
        keywords = "vpn settings, endpoint, allowed ips, mtu",
        on = VpnProfileRow
    )]
    Edit {
        #[param("What the profile is called")]
        name: Option<String>,
        #[param("Whether it connects by itself")]
        autoconnect: Option<bool>,
        #[param("Whether all traffic goes through it")]
        all_traffic: Option<bool>,
        #[param("Name servers it uses")]
        dns: Option<Vec<String>>,
        #[param("The largest packet in bytes; 0 chooses it automatically")]
        mtu: Option<u32>,
        #[param("The server it connects to, host:port")]
        endpoint: Option<String>,
        #[param("The addresses sent through it")]
        allowed_ips: Option<Vec<String>>,
        #[param("Seconds between keepalives; 0 sends none")]
        keepalive: Option<u32>,
        #[param("Whether its traffic is routed over Tor")]
        tor: Option<bool>,
    },
    /// Dismiss what the last attempt to bring it up said.
    #[effect(kind = "outside")]
    #[effect(own_device, row, icon = "x", on = VpnProfileRow)]
    Dismiss,
    /// Remove the profile from this device.
    #[effect(kind = "outside")]
    #[effect(own_device, row, destructive, icon = "trash-2", on = VpnProfileRow)]
    Forget,
    /// The VPN as a configuration file's text, which importing it elsewhere reads: its keys go
    /// with it.
    #[effect(
        kind = "read",
        returns = VpnConfiguration,
        icon = "download",
        keywords = "vpn export, save configuration",
        on = VpnProfileRow
    )]
    Export,
}

/// The VPN's configuration file, as text: what exporting it answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VpnConfiguration;

impl tilo_resource::JsonSchema for VpnConfiguration {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({ "type": "string" })
    }
}

/// The `tethering` resource's actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.tethering")]
pub enum TetheringAction {
    /// Share this device's connection over a Wi-Fi hotspot, or stop.
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
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

/// The `modem` resource's actions: what the modems and their SIMs offer.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.connectivity.modem")]
pub enum ModemAction {
    /// Read the modems and their SIMs again.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "refresh-cw",
        keywords = "modem, sim, mobile network"
    )]
    RefreshModems,
    /// Unlock a SIM with its PIN.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "lock", keywords = "sim pin, unlock sim")]
    UnlockSim {
        #[param("The SIM, as listed; the locked one when omitted")]
        sim: Option<String>,
        #[param(secret, "The SIM's PIN")]
        pin: Option<String>,
    },
    /// Unblock a SIM with its PUK, choosing a new PIN.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "unlock", keywords = "sim puk, unblock sim")]
    UnblockSim {
        #[param("The SIM, as listed; the blocked one when omitted")]
        sim: Option<String>,
        #[param(secret, "The PUK from the SIM card's package")]
        puk: Option<String>,
        #[param(secret, "The new PIN")]
        pin: Option<String>,
    },
    /// Ask for the SIM's PIN when the device starts, or stop asking.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent, icon = "lock", keywords = "sim pin lock")]
    SetSimPinLock {
        #[param("The SIM, as listed; the one in use when omitted")]
        sim: Option<String>,
        #[param("Whether the PIN is asked for at startup")]
        on: bool,
        #[param(secret, "The SIM's PIN")]
        pin: Option<String>,
    },
    /// Change the SIM's PIN.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "lock", keywords = "change sim pin")]
    ChangeSimPin {
        #[param("The SIM, as listed; the one in use when omitted")]
        sim: Option<String>,
        #[param(secret, "The current PIN")]
        old_pin: Option<String>,
        #[param(secret, "The new PIN")]
        new_pin: Option<String>,
    },
    /// Use a SIM for mobile data, on a modem with several.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "signal", keywords = "sim slot, switch sim")]
    UseSim {
        #[param("The SIM, as listed")]
        sim: String,
    },
    /// Choose the network types the modem may use.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "signal",
        keywords = "network type, 5g, 4g"
    )]
    SetPreferredNetwork {
        #[param("The modem, as listed; the first when omitted")]
        modem: Option<String>,
        #[param("One of the keys the modem lists: 5g-4g-3g, 4g-3g-2g, …")]
        network: String,
    },
    /// Look for the operators the modem can reach, and list them.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "search",
        keywords = "operators, scan, manual network selection"
    )]
    ScanOperators {
        #[param("The modem, as listed; the first when omitted")]
        modem: Option<String>,
    },
    /// Stand on an operator's network, or choose the one this device uses automatically.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "signal",
        keywords = "operator, network selection, automatic"
    )]
    SelectOperator {
        #[param("The modem, as listed; the first when omitted")]
        modem: Option<String>,
        #[param("The operator's MCC and MNC, as a scan lists it; nothing chooses automatically")]
        operator: Option<String>,
    },
    /// Send a code the operator answers, such as `*100#` for a balance.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "menu",
        keywords = "ussd, code, balance, top up, prepaid"
    )]
    SendCode {
        #[param("The modem, as listed; the first when omitted")]
        modem: Option<String>,
        #[param("The code, such as *100#")]
        code: String,
    },
    /// Answer the question the operator's code asked.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "menu", keywords = "ussd, answer, menu")]
    AnswerCode {
        #[param("The modem, as listed; the first when omitted")]
        modem: Option<String>,
        #[param("The answer it asks for")]
        answer: String,
    },
    /// End the code's session with the operator.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "menu", keywords = "ussd, end, cancel")]
    CancelCode {
        #[param("The modem, as listed; the first when omitted")]
        modem: Option<String>,
    },
}
