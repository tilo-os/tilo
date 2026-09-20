//! Explicit absence of Linux evdev on other hosts; remote forwarding remains available.
use crate::ForwardedGamepadEvent;
use std::{io, path::PathBuf};
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysicalGamepadId(PathBuf);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalGamepadInfo {
    pub id: PhysicalGamepadId,
    pub name: String,
}
#[derive(Clone, Debug, Default)]
pub struct LinuxPhysicalGamepadCatalog;
impl LinuxPhysicalGamepadCatalog {
    pub fn discover(&self) -> io::Result<Vec<PhysicalGamepadInfo>> {
        Ok(Vec::new())
    }
    pub fn open(&self, _: &PhysicalGamepadId) -> io::Result<LinuxPhysicalGamepadSource> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux evdev is unavailable on this host",
        ))
    }
}
pub struct LinuxPhysicalGamepadSource {
    _private: (),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalGamepadDrain {
    pub events: Vec<ForwardedGamepadEvent>,
    pub saturated: bool,
}
impl LinuxPhysicalGamepadSource {
    pub fn drain(&mut self, _: usize) -> io::Result<PhysicalGamepadDrain> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux evdev is unavailable on this host",
        ))
    }
}
