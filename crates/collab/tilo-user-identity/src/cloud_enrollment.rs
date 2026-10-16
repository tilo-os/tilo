//! The capability set a hosting machine's certificate carries.

use crate::DeviceCapabilities;

/// Narrow host delegation: directory ownership and hosting, without device admission.
pub fn host_capabilities() -> DeviceCapabilities {
    DeviceCapabilities {
        can_own_workspaces: true,
        can_admit_devices: false,
        can_act_as_host: true,
        can_read_all_workspaces: true,
        foreign: false,
        can_administer_devices: false,
    }
}
