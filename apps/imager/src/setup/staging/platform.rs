//! Compatibility facade for the native staging backend.

pub use imager_backend::platform::{
    Disk, IoResult, LinuxBtrfsVolume, NewPartition, Partition, Platform, SystemVolume,
};
