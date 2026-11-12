//! The capability set a hosting machine's certificate carries.

use crate::DeviceCapabilities;

/// Narrow host delegation: directory ownership and hosting, without device admission.
pub fn host_capabilities() -> DeviceCapabilities {
    DeviceCapabilities {
        can_own_workspaces: true,
        ..DeviceCapabilities::host()
    }
}
