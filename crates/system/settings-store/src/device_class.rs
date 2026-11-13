//! What kind of device a process's settings are read on, for the settings whose default differs by
//! it: on by default where the device is the person's own computer, off where it is a page in a
//! browser.
//!
//! The host says which: a native host leaves the default, and the web runtime installs
//! [`DeviceClass::Web`] before it starts an app. A setting declares a default per class
//! (`SettingsSchema::defaults_on`).

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DeviceClass {
    #[default]
    Native,
    Web,
}

static INSTALLED: AtomicU8 = AtomicU8::new(0);

impl DeviceClass {
    #[must_use]
    pub fn current() -> Self {
        match INSTALLED.load(Ordering::Acquire) {
            1 => Self::Web,
            _ => Self::Native,
        }
    }

    pub fn install(self) {
        let value = match self {
            Self::Native => 0,
            Self::Web => 1,
        };
        INSTALLED.store(value, Ordering::Release);
    }
}
