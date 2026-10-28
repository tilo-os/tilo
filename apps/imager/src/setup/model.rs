//! Pure, non-privileged setup choices and probed capabilities.
//!
//! Values in this module describe user intent and probed capabilities. They are
//! local application state, not authorization for installation or file access.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOs {
    Windows,
    Linux,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Experience {
    Desktop,
    FullOs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderMode {
    Copy,
    Connected,
    Later,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub host: HostOs,
    pub desktop_session: bool,
    pub internal_dual_boot: bool,
}

impl Capabilities {
    #[must_use]
    pub fn supports(&self, experience: Experience) -> bool {
        match (self.host, experience) {
            (HostOs::Linux, Experience::Desktop) => self.desktop_session,
            (HostOs::Windows | HostOs::Linux, Experience::FullOs) => self.internal_dual_boot,
            _ => false,
        }
    }
}
