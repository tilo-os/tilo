//! What kind of device a process's settings are read on, for the settings
//! whose default differs by it. A setting that keeps a copy of something on
//! the device or works in the background is on by default where the device
//! is the person's own computer, and off where it is a page in a browser,
//! whose storage and network the browser lends.
//!
//! The host says which: a native host leaves the default, the web runtime
//! installs [`DeviceClass::Web`] before it starts an app. Apps never ask
//! which target they were compiled for; a setting declares a default per
//! class (`SettingsSchema::defaults_on`).

use std::sync::atomic::{AtomicU8, Ordering};

/// The kinds of device a setting's default may differ on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DeviceClass {
    /// The person's own computer, running Tilo or a native Tilo app.
    #[default]
    Native,
    /// A page in a browser: the web runtime.
    Web,
}

static INSTALLED: AtomicU8 = AtomicU8::new(0);

impl DeviceClass {
    /// The class the host installed; native unless it installed another.
    #[must_use]
    pub fn current() -> Self {
        match INSTALLED.load(Ordering::Acquire) {
            1 => Self::Web,
            _ => Self::Native,
        }
    }

    /// Make this the class of the device the process runs on.
    pub fn install(self) {
        let value = match self {
            Self::Native => 0,
            Self::Web => 1,
        };
        INSTALLED.store(value, Ordering::Release);
    }
}
