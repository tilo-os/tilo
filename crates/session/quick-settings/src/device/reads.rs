//! What the panel reads of the device, one read each, and what a read
//! found.
//!
//! The reads go through the OS (`NetworkManager`, `BlueZ`, the power daemon)
//! and some take seconds, so they only ever run on a worker
//! ([`super::refresh`]). [`DeviceReads`] is the seam: the desktop reads the
//! OS ([`OsReads`]), a test reads stand-ins. The sound, the brightness, the
//! battery and what holds the machine awake are not read here: the audio
//! service, the session's displays and the power service own them, and the
//! session hands the panel what they read ([`super::DeviceHost::sound`],
//! [`super::DeviceHost::update`]).

use os_actions as qs;

use crate::actions::QsActionAvailability;
use crate::i18n::t;

/// The device's state, as the panel reads it. Each read may take seconds
/// and is only ever made on a worker; `None` (or an error) means it could
/// not be read, which leaves what the panel shows as it is.
pub trait DeviceReads: Send + Sync {
    /// Whether Bluetooth is on; `None` without an adapter.
    fn bluetooth(&self) -> Option<bool>;
    fn power(&self) -> Option<qs::PowerProfile>;
    /// The profiles the power daemon offers.
    fn power_profiles(&self) -> Option<Vec<qs::PowerProfile>>;
    fn wired(&self) -> Option<qs::WiredInfo>;
    /// One snapshot of every radio; Wi-Fi on or off is taken from it.
    fn radios(&self) -> Option<qs::RadioState>;
    fn vpns(&self) -> Option<Vec<qs::VpnConnectionInfo>>;
    /// The link the machine is online through, as the top bar names it.
    fn link(&self) -> Option<qs::LinkStatus>;
    /// The networks `NetworkManager` knows now, without scanning.
    ///
    /// # Errors
    /// Why they could not be listed.
    fn wifi_networks(&self) -> Result<Vec<qs::WifiNetworkInfo>, String>;
    /// Look for networks again; slow on some adapters.
    ///
    /// # Errors
    /// Why the scan failed.
    fn scan_wifi(&self) -> Result<(), String>;
    /// The Wi-Fi network profiles `NetworkManager` keeps, in reach or not.
    fn saved_networks(&self) -> Vec<qs::SavedNetworkProfile> {
        Vec::new()
    }
    fn bt_devices(&self) -> Option<Vec<qs::BtDeviceInfo>>;
    /// The Bluetooth devices in reach that are not paired, strongest first.
    fn nearby_bluetooth(&self) -> Vec<qs::NearbyBtDevice> {
        Vec::new()
    }
    /// Whether `BlueZ` is looking for devices now.
    fn scanning_bluetooth(&self) -> Option<bool> {
        None
    }
    /// Which controls the machine has hardware for, beyond what the other
    /// reads show.
    fn availability(&self) -> QsActionAvailability;
}

/// The device, read through the OS.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsReads;

impl DeviceReads for OsReads {
    fn bluetooth(&self) -> Option<bool> {
        qs::log_get("get_bluetooth", qs::bluez::is_powered())
    }

    fn power(&self) -> Option<qs::PowerProfile> {
        qs::log_get("get_power_profile", qs::get_power_profile())
    }

    fn power_profiles(&self) -> Option<Vec<qs::PowerProfile>> {
        qs::log_get("available_power_profiles", qs::available_power_profiles())
    }

    fn wired(&self) -> Option<qs::WiredInfo> {
        qs::log_get("get_wired", qs::get_wired())
    }

    fn radios(&self) -> Option<qs::RadioState> {
        qs::log_get("get_radio_state", qs::get_radio_state())
    }

    fn vpns(&self) -> Option<Vec<qs::VpnConnectionInfo>> {
        qs::log_get("list_vpn_connections", qs::list_vpn_connections())
    }

    fn link(&self) -> Option<qs::LinkStatus> {
        qs::log_get("get_link_status", qs::get_link_status())
    }

    fn wifi_networks(&self) -> Result<Vec<qs::WifiNetworkInfo>, String> {
        qs::list_wifi_networks()
    }

    fn scan_wifi(&self) -> Result<(), String> {
        qs::scan_wifi()
    }

    fn saved_networks(&self) -> Vec<qs::SavedNetworkProfile> {
        qs::log_get(
            "list_saved_network_profiles",
            qs::list_saved_network_profiles(),
        )
        .unwrap_or_default()
        .into_iter()
        .filter(|profile| profile.kind == "802-11-wireless")
        .collect()
    }

    fn bt_devices(&self) -> Option<Vec<qs::BtDeviceInfo>> {
        qs::log_get("list_bt_devices", qs::list_bt_devices())
    }

    fn nearby_bluetooth(&self) -> Vec<qs::NearbyBtDevice> {
        qs::log_get("nearby_bluetooth", qs::nearby_bluetooth()).unwrap_or_default()
    }

    fn scanning_bluetooth(&self) -> Option<bool> {
        qs::log_get("scanning_bluetooth", qs::scanning_bluetooth())
    }

    fn availability(&self) -> QsActionAvailability {
        QsActionAvailability::detect()
    }
}

/// Everything the panel shows, read once on opening. Every field is
/// independently optional: `None` means "not read", which leaves the
/// panel's value alone rather than blanking it — a missing daemon, or a
/// field the panel wrote after this read had started.
#[derive(Debug)]
pub(crate) struct FastReading {
    pub(crate) bluetooth: Option<bool>,
    /// A successful `BlueZ` state query proves that an adapter exists.
    pub(crate) bluetooth_available: bool,
    pub(crate) power: Option<qs::PowerProfile>,
    pub(crate) power_profiles: Option<Vec<qs::PowerProfile>>,
    pub(crate) wired: Option<qs::WiredInfo>,
    pub(crate) radios: Option<qs::RadioState>,
    pub(crate) vpns: Option<Vec<qs::VpnConnectionInfo>>,
    pub(crate) link: Option<qs::LinkStatus>,
    pub(crate) available: QsActionAvailability,
}

impl FastReading {
    /// Read everything through `reads`, once each.
    pub(crate) fn read(reads: &dyn DeviceReads) -> Self {
        let bluetooth = reads.bluetooth();
        Self {
            bluetooth_available: bluetooth.is_some(),
            bluetooth,
            power: reads.power(),
            power_profiles: reads.power_profiles(),
            wired: reads.wired(),
            radios: reads.radios(),
            vpns: reads.vpns(),
            link: reads.link(),
            available: reads.availability(),
        }
    }

    /// Drop what `reader` produced: the panel wrote that state after this
    /// read started, so landing it would flip the control back, and the next
    /// press would then repeat the change instead of reversing it.
    pub(crate) fn forget(&mut self, reader: Reader) {
        match reader {
            Reader::Radios => self.radios = None,
            Reader::Bluetooth => self.bluetooth = None,
            Reader::Wired => self.wired = None,
            Reader::Power => self.power = None,
            Reader::Vpns => self.vpns = None,
        }
    }

    /// A reading from a machine with none of these devices.
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            bluetooth: None,
            bluetooth_available: false,
            power: None,
            power_profiles: None,
            wired: None,
            radios: None,
            vpns: None,
            link: None,
            available: QsActionAvailability::default(),
        }
    }
}

/// A control the panel writes, named so the read-back after the write
/// covers that control and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum QsField {
    Wifi,
    Bluetooth,
    Wired,
    Airplane,
    Mobile,
    Power,
    Vpn,
}

impl QsField {
    /// Wi-Fi, airplane mode and mobile broadband are all one radio
    /// snapshot.
    pub(crate) const fn reader(self) -> Reader {
        match self {
            Self::Wifi | Self::Airplane | Self::Mobile => Reader::Radios,
            Self::Bluetooth => Reader::Bluetooth,
            Self::Wired => Reader::Wired,
            Self::Power => Reader::Power,
            Self::Vpn => Reader::Vpns,
        }
    }

    /// The control a radio change writes.
    pub(crate) const fn of_radio(change: qs::RadioChange) -> Self {
        match change {
            qs::RadioChange::Wifi(_) => Self::Wifi,
            qs::RadioChange::Airplane(_) => Self::Airplane,
        }
    }
}

/// The OS read behind one or more [`QsField`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Reader {
    Radios,
    Bluetooth,
    Wired,
    Power,
    Vpns,
}

impl Reader {
    pub(crate) const ALL: [Self; 5] = [
        Self::Radios,
        Self::Bluetooth,
        Self::Wired,
        Self::Power,
        Self::Vpns,
    ];

    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    /// Read back what this reader covers.
    pub(crate) fn read(self, reads: &dyn DeviceReads) -> FieldReading {
        match self {
            Self::Radios => FieldReading::Radios(reads.radios()),
            Self::Bluetooth => FieldReading::Bluetooth(reads.bluetooth()),
            Self::Wired => FieldReading::Wired(reads.wired()),
            Self::Power => FieldReading::Power(reads.power()),
            Self::Vpns => FieldReading::Vpns(reads.vpns()),
        }
    }
}

/// What one field's read-back found. `None` leaves the panel's value alone.
#[derive(Debug)]
pub(crate) enum FieldReading {
    Radios(Option<qs::RadioState>),
    Bluetooth(Option<bool>),
    Wired(Option<qs::WiredInfo>),
    Power(Option<qs::PowerProfile>),
    Vpns(Option<Vec<qs::VpnConnectionInfo>>),
}

impl FieldReading {
    pub(crate) const fn reader(&self) -> Reader {
        match self {
            Self::Radios(_) => Reader::Radios,
            Self::Bluetooth(_) => Reader::Bluetooth,
            Self::Wired(_) => Reader::Wired,
            Self::Power(_) => Reader::Power,
            Self::Vpns(_) => Reader::Vpns,
        }
    }
}

/// A join of a Wi-Fi network, as it ended.
#[derive(Debug)]
pub(crate) struct WifiJoinResult {
    pub(crate) ssid: String,
    pub(crate) password: Option<String>,
    pub(crate) result: Result<(), String>,
}

/// How a failed write is told: its title, and whether the control went
/// back to the state or the value the device has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// A switch, put back as the device has it.
    State(String),
    /// The device's own reason, while its state is read again.
    Refreshing(String),
}

impl Failure {
    /// The notification's title and body for `error`.
    pub(crate) fn told(self, error: &str) -> (String, String) {
        match self {
            Self::State(title) => (title, t("host-qs-change-failed-state-restored")),
            Self::Refreshing(title) => (
                title,
                crate::i18n::t_args("qs-action-failed-refreshing", &[("error", error.into())]),
            ),
        }
    }
}

/// What is read again once a write is done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadBack {
    Field(QsField),
    /// The accessories, which are `BlueZ`'s.
    Accessories,
    /// The networks and the saved profiles, which are `NetworkManager`'s.
    Networks,
}

/// A write the panel made, as it ended.
#[derive(Debug)]
pub(crate) struct WriteDone {
    pub(crate) operation: &'static str,
    pub(crate) failure: Failure,
    pub(crate) read_back: ReadBack,
    pub(crate) result: Result<(), String>,
}

/// What the panel lists of Bluetooth: the paired accessories, the devices
/// in reach, and whether `BlueZ` is looking for more.
#[derive(Debug)]
pub(crate) struct BluetoothReading {
    pub(crate) paired: Option<Vec<qs::BtDeviceInfo>>,
    pub(crate) nearby: Vec<qs::NearbyBtDevice>,
    pub(crate) scanning: Option<bool>,
}

/// What the panel lists of Wi-Fi: the networks in reach as
/// `NetworkManager` knows them, and the profiles it keeps to join them
/// again.
#[derive(Debug)]
pub(crate) struct WifiReading {
    pub(crate) networks: Result<Vec<qs::WifiNetworkInfo>, String>,
    pub(crate) saved: Vec<qs::SavedNetworkProfile>,
}

/// What a worker hands back.
#[derive(Debug)]
pub(crate) enum QsReading {
    Fast(Box<FastReading>),
    Field(FieldReading),
    WifiNetworks(WifiReading),
    BtDevices(BluetoothReading),
    WifiJoin(WifiJoinResult),
    Wrote(WriteDone),
}
