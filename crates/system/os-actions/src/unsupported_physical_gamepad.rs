//! Explicit absence of Linux evdev on other hosts; remote forwarding remains available.
use crate::ForwardedGamepadEvent;
use std::{io, os::fd::OwnedFd, path::PathBuf};
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Linux evdev is unavailable on this host",
    )
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysicalGamepadId(PathBuf);
impl PhysicalGamepadId {
    /// The controller at `path`, the way discovery names the ones it finds.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalGamepadInfo {
    pub id: PhysicalGamepadId,
    pub name: String,
    pub vendor_id: Option<u16>,
}
#[derive(Clone, Debug, Default)]
pub struct LinuxPhysicalGamepadCatalog;
impl LinuxPhysicalGamepadCatalog {
    pub fn discover(&self) -> io::Result<Vec<PhysicalGamepadInfo>> {
        Ok(Vec::new())
    }
    pub fn open(&self, _: &PhysicalGamepadId) -> io::Result<LinuxPhysicalGamepadSource> {
        Err(unsupported())
    }
    pub fn hotplug_watch(&self) -> io::Result<PhysicalGamepadHotplug> {
        Err(unsupported())
    }
}
pub struct PhysicalGamepadHotplug {
    _private: (),
}
impl PhysicalGamepadHotplug {
    pub fn wake_fd(&self) -> io::Result<OwnedFd> {
        Err(unsupported())
    }
    pub fn take_changes(&mut self) -> bool {
        false
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
    pub fn wake_fd(&self) -> io::Result<OwnedFd> {
        Err(unsupported())
    }
    pub fn drain(&mut self, _: usize) -> io::Result<PhysicalGamepadDrain> {
        Err(unsupported())
    }
}
