//! The platform's published resources as a tilolang process reads them: this device's readings
//! (its journal, its disks, its sound), Settings' and the workspace's activity, followed where
//! the session publishes them over the process's collaboration connection.
//!
//! A host a device starts reads them as it runs its apps' instances ([`Session`] installed at its
//! start). A window that runs an instance in its own process does too — a capture's window, which
//! joins its workspace over the connection the device made for it — so it installs the same
//! source, or every platform root would read the type's own defaults and say the device publishes
//! nothing.

use tilolang_tilo::platform_apps::{PublishedSource, Reading, Seen};

/// The session's services: the platform roots read where the session publishes them.
pub struct Session;

impl PublishedSource for Session {
    fn follow(
        &self,
        resource: &str,
        wake: tilo_rt::wake::WakeHandle,
    ) -> Result<Box<dyn Reading>, String> {
        Ok(Box::new(Followed(collaboration::SessionResource::follow(
            resource, wake,
        )?)))
    }

    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        collaboration::SessionResource::invoke_without_watching(resource, action, arguments)
    }
}

/// A resource of the session's followed: each reading the session publishes.
struct Followed(collaboration::SessionResource);

impl Reading for Followed {
    fn latest(&self) -> Option<Seen> {
        Some(match self.0.latest()? {
            collaboration::PublishedReading::Reading(reading) => Seen::Reading(reading),
            collaboration::PublishedReading::Pending => Seen::Pending,
            collaboration::PublishedReading::Failed(why) => Seen::Unreadable(why),
        })
    }
}

/// Install the session's published resources as this process's, once.
pub fn install() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| tilolang_tilo::platform_apps::install(Box::new(Session)));
}
