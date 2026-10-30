//! The system update: what the update service staged, and what finishing
//! it needs.
//!
//! The update service fetches and stages the newest system image on its
//! own schedule, and a staged image applies live; what waits for the
//! desktop or a restart of the machine is in the live update's state
//! ([`LIVE_STATE`]). Where no live update runs, an image staged at all
//! needs a restart.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// What the live update applied of the staged image, and what waits.
pub const LIVE_STATE: &str = "/run/tilo/live-update/state.json";
/// Where ostree records the deployment staged for the next boot.
const OSTREE_STAGED: &str = "/run/ostree/staged-deployment";
/// The unit that fetches and stages the newest image now, which the person
/// at the machine may start (os/etc/polkit-1/rules.d/49-tilo-updates.rules).
pub const UPDATE_UNIT: &str = "tilo-deployment-update.service";
/// How long fetching, staging and applying an image may take.
pub const UPDATE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// What the live update says of the staged image.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct LiveState {
    /// The staged image it describes; empty when nothing is staged.
    #[serde(default)]
    pub image: String,
    /// Part of the update waits for the desktop to restart.
    #[serde(default)]
    pub desktop_pending: bool,
    /// Part of the update waits for a restart of the machine.
    #[serde(default)]
    pub reboot_required: bool,
}

/// The `system-update` resource.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Update {
    /// The staged image, when the live update names it.
    pub staged: Option<String>,
    /// Part of it waits for the desktop to restart.
    pub desktop_pending: bool,
    /// The machine must restart to finish it.
    pub restart_needed: bool,
    /// An update asked for here is being fetched and applied.
    pub updating: bool,
    pub pending: Vec<String>,
    pub problem: Option<String>,
}

impl Update {
    /// The reading of the live update's state `live`, else of whether an
    /// image is `staged` at all.
    #[must_use]
    pub fn of(staged: bool, live: Option<&LiveState>) -> Self {
        Self {
            staged: live
                .map(|state| state.image.clone())
                .filter(|image| !image.is_empty()),
            desktop_pending: live.is_some_and(|state| state.desktop_pending),
            restart_needed: restart_needed(staged, live),
            ..Self::default()
        }
    }
}

/// Whether the machine needs a restart for the staged update: what the live
/// update says, or, where none runs, that an image is staged at all.
#[must_use]
pub fn restart_needed(staged: bool, live: Option<&LiveState>) -> bool {
    live.map_or(staged, |state| state.reboot_required)
}

/// The live update's state, where one runs.
#[must_use]
pub fn live_state() -> Option<LiveState> {
    live_state_in(&std::fs::read_to_string(LIVE_STATE).ok()?)
}

fn live_state_in(json: &str) -> Option<LiveState> {
    serde_json::from_str(json).ok()
}

/// Whether an image waits for the next restart.
#[must_use]
pub fn staged() -> bool {
    if Path::new(OSTREE_STAGED).exists() {
        return true;
    }
    bounded_command::run(
        "bootc",
        &["status", "--format", "json"],
        None,
        1024 * 1024,
        Duration::from_secs(10),
    )
    .ok()
    .filter(|output| output.status.success())
    .is_some_and(|output| staged_in(&String::from_utf8_lossy(&output.stdout)))
}

/// Whether `bootc status --format json` reports a staged deployment.
fn staged_in(json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|status| status.get("status")?.get("staged").cloned())
        .is_some_and(|staged| !staged.is_null())
}

/// Fetch, stage and apply the newest system now: the update unit returns
/// once the image is staged and applied live, or there was nothing newer.
///
/// # Errors
/// The unit did not run, or failed.
pub fn update_now() -> Result<(), String> {
    let output = bounded_command::run(
        "systemctl",
        &["start", "--no-ask-password", UPDATE_UNIT],
        None,
        64 * 1024,
        UPDATE_TIMEOUT,
    )
    .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("the system update ended with {}", output.status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_update_decides_whether_the_machine_restarts() {
        let live = |desktop_pending, reboot_required| LiveState {
            image: "ghcr.io/tilo/os:42".into(),
            desktop_pending,
            reboot_required,
        };
        assert!(!restart_needed(false, None));
        assert!(restart_needed(true, None), "staged with no live update");
        assert!(
            !restart_needed(true, Some(&live(true, false))),
            "applied live: the desktop finishes it"
        );
        assert!(restart_needed(false, Some(&live(false, true))));
        let update = Update::of(true, Some(&live(true, false)));
        assert_eq!(update.staged.as_deref(), Some("ghcr.io/tilo/os:42"));
        assert!(update.desktop_pending && !update.restart_needed);
    }

    #[test]
    fn states_read_from_their_json() {
        assert_eq!(
            live_state_in(r#"{"image":"x","desktop_pending":true}"#),
            Some(LiveState {
                image: "x".into(),
                desktop_pending: true,
                reboot_required: false
            })
        );
        assert!(live_state_in("not json").is_none());
        assert!(staged_in(r#"{"status":{"staged":{"image":"x"}}}"#));
        assert!(!staged_in(r#"{"status":{"staged":null}}"#));
        assert!(!staged_in("{}"));
    }
}
