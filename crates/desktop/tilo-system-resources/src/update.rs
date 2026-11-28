//! The `system-update` resource: its actions' ids and its reading.

use serde::{Deserialize, Serialize};

/// The `system-update` action that fetches and applies the newest system now.
pub const UPDATE_NOW: &str = "update-now";
/// The `system-update` action that restarts the desktop onto the new
/// interface to finish the update.
pub const RESTART_DESKTOP: &str = "restart-desktop";
/// The `system-update` action that restarts the machine into the staged
/// update.
pub const RESTART_TO_UPDATE: &str = "restart-to-update";
/// The `system-update` action that boots the previous deployment from now on
/// and restarts the machine into it.
pub const ROLLBACK: &str = "rollback";

/// One deployment of the system: the image it was made from.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Deployment {
    /// The image as the update service fetches it (`registry.tilo.so/tilo/tilo-vm:x86_64-ci`).
    pub image: Option<String>,
    /// The image's digest: what names it to the live update.
    pub digest: Option<String>,
    /// The version the image carries, where it carries one.
    pub version: Option<String>,
    /// When the image was built, as RFC 3339.
    pub built: Option<String>,
}

impl Deployment {
    /// What a person calls it: its version, else its image.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.version.as_deref().or(self.image.as_deref())
    }

    /// The day it was built (`2026-11-21`).
    #[must_use]
    pub fn built_on(&self) -> Option<&str> {
        self.built.as_deref().and_then(|built| built.get(..10))
    }
}

/// A deployment this machine went back from: it did not start within its
/// tries, so the boot loader started the previous one and the machine settled
/// on it. What the boot-time settling recorded of it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct Fallback {
    /// The report's directory name, `<when>-<composefs>`, which names the
    /// fallback once per machine: the session tells each of them once.
    pub id: String,
    /// The image that did not start.
    pub image: Option<String>,
    /// Its version, where the image has one.
    pub version: Option<String>,
    /// How many times it was tried.
    pub tries: Option<usize>,
    /// The version the machine runs instead.
    pub running: Option<String>,
    /// The report a person reads of what the failed tries left behind.
    pub summary: String,
}

/// The `system-update` resource.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a reading of its own that a person's update page shows beside the others, not a state one of them implies"
)]
pub struct Update {
    /// The system this machine started; `None` where its admins alone may see it.
    pub booted: Option<Deployment>,
    /// The update staged for the next start, where one is. Where the deployments cannot be seen,
    /// only that one is staged, or the digest the live update names.
    pub staged: Option<Deployment>,
    /// What this machine can go back to; `None` where nothing is, or its admins alone may see it.
    pub rollback: Option<Deployment>,
    /// The staged update applied live in full: it runs now, and nothing of it waits.
    pub applied: bool,
    /// Part of it waits for the desktop to restart.
    pub desktop_pending: bool,
    /// The machine must restart to finish it.
    pub restart_needed: bool,
    /// Applying the staged update live did not finish and was taken back, so the machine runs
    /// what it ran before and a restart finishes the update: why, in the live update's words.
    pub failed: Option<String>,
    /// An update asked for here is being fetched and applied.
    pub updating: bool,
    pub pending: Vec<String>,
    pub problem: Option<String>,
    /// Firmware updates `fwupd` offers; `None` where it is not installed.
    pub firmware: Option<usize>,
    /// The deployments this machine went back from, oldest first.
    pub fell_back: Vec<Fallback>,
    /// When this machine updates by itself: `system` (the image's own schedule), `daily` or
    /// `weekly`; `None` where the machine keeps no update policy it can read.
    pub schedule: Option<String>,
    /// Whether an automatic update may download over a metered connection; `None` where the
    /// machine keeps no update policy it can read.
    pub allow_metered: Option<bool>,
}
