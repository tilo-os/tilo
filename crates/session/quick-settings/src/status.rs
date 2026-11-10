//! The device status Quick Settings draws: battery, link, volume and capture streams, as last
//! read.

use connectivity::model::Link;
use media::audio::default_node::VolumeState;
use tilo_power_mgmt::BatterySnapshot;

/// Every field is `Option` because "not read yet" and "this machine
/// has none" are both real and both drawn as absence, not a zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemStatus {
    /// `Some` with `present == false` on a machine with no battery.
    pub battery: Option<BatterySnapshot>,
    /// Time to empty or to full, when the battery reports its rate.
    pub battery_time: Option<tilo_power_mgmt::BatteryTime>,
    pub link: Option<Link>,
    pub volume: Option<VolumeState>,
    /// Active capture streams, attributed by the audio service from
    /// `PipeWire`'s server security PID to the kernel executable, and read
    /// again whenever the set of running streams changes.
    pub media: Vec<os_actions::ActiveMediaUse>,
}
