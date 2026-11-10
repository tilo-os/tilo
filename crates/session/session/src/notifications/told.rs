//! The person's notices as a test host shows them, for a test to read back.

use tilo_desktop_surface::DesktopNotifications;

use crate::platform::notice::{Notice, Notices};

/// A test host's notices, on a desk of its own.
#[derive(Clone, Default)]
pub(crate) struct Told(pub(crate) DesktopNotifications);

impl Told {
    /// What the desk shows, live first: each one's title and body.
    pub(crate) fn told(&self) -> Vec<(String, String)> {
        let log = self.0.log();
        log.active()
            .iter()
            .chain(log.history())
            .map(|notification| (notification.title.clone(), notification.body.clone()))
            .collect()
    }
}

impl Notices for Told {
    fn show(&self, notice: &Notice) {
        self.0.post(notice.shown());
    }

    fn withdraw(&self, _tag: &str) {}
}
