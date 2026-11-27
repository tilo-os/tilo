//! Native USB-free installation protocols and disk/firmware adapters.
//!
//! Portable parsing and sequencing remain available on every host. Native
//! entry points refuse unsupported hosts rather than acquiring authority there.

pub mod btrfs_resize;
pub mod capacity_probe;
pub mod fat;
pub mod final_bootnext;
pub mod layout;
pub mod process;
pub mod protocol;
pub mod record_io;
pub mod request_io;
pub mod target;
pub mod windows;

pub use target::TargetDisk;

/// Read native installation facts without granting write authority.
/// Unsupported hosts return an explicit blocker rather than hiding the flow.
pub fn probe_host() -> crate::backend::model::HostFacts {
    #[cfg(target_os = "linux")]
    {
        crate::backend::linux::probe_facts()
    }
    #[cfg(windows)]
    {
        windows::probe_facts()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        crate::backend::model::HostFacts::unsupported(
            crate::backend::model::Blocker::DiskUnsupported,
        )
    }
}

pub use crate::backend::model as probe;
pub use crate::backend::{ESP_TYPE, is_guid, random_guid, same_guid};
pub use crate::backend::{plan, platform};

/// GPT type reserved for the temporary installer root.
pub const STAGING_ROOT_TYPE: &str = staging_handoff_model::gpt::TILO_STAGING_ROOT;
