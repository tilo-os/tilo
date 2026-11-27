//! How far setup got on this device: the step a restart opens setup at again, so the person is
//! never sent past a step they had not finished, nor asked again for one they had.
//!
//! Until this device holds an identity there is no person to keep it for, and the step is only
//! the view's own, in memory. A restart then finds no identity and setup starts over from its
//! first page: nothing the person chose before an identity exists is kept anywhere, so there is
//! nothing to redo behind their back.
//!
//! From the identity on, the step is a field of the person's settings. Their [`SECTION`] holds
//! one leaf for each of their devices whose setup is unfinished, named by the device's public
//! key, so the setup one of their devices is in never reopens on another; finishing removes the
//! leaf. The settings carry it to the person's record in their own workspace with every other
//! setting the person holds.

use serde_json::{Map, Value};
use tilo_device_identity::IdentityPaths;

/// The section of the person's settings that holds the unfinished setup of each of their devices.
pub const SECTION: &str = "onboarding";

/// What the settings history says wrote a step.
const ORIGIN: &str = "onboarding/progress";

/// The step setup on this device reached, once it holds an identity.
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Stage {
    /// The identity exists and setup must still offer a portable backup.
    IdentityReady,
    /// A backup was verified, but setup has not reached its hand-off.
    IdentityReadyBackedUp,
    /// The person handled the backup choice and reached the final hand-off.
    ReadyToEnter,
    /// The hand-off, reached after a backup was verified. Only that fact is kept, never where
    /// the backup went.
    ReadyToEnterBackedUp,
    TourNavigate,
    TourOrganise,
    TourCollaborate,
    TourStart,
}

/// This device's leaf in [`SECTION`]: its public key, as the device's daemon holds it: the key
/// it took, in the daemon's own process, and otherwise its answer. A computer whose daemon does
/// not answer has no leaf.
pub(super) fn leaf(paths: &IdentityPaths) -> std::io::Result<String> {
    let key = tilo_device_identity::persist::device_public_key(paths)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(hex::encode(key))
}

/// The step the person's settings hold for this device: `None` when its setup is finished or
/// was never begun, and when this device has no key yet, as before its identity is made. A value
/// this build cannot read is an `Err`: setup is under way, at a step nobody can name.
pub(super) fn recorded(paths: &IdentityPaths) -> Option<Result<Stage, String>> {
    let leaf = leaf(paths).ok()?;
    match settings_store::load_section(SECTION).remove(&leaf)? {
        Value::Null => None,
        value => Some(serde_json::from_value(value).map_err(|error| error.to_string())),
    }
}

/// Keep `stage` as this device's step in the person's settings, or with `None` remove it: setup
/// here is finished.
///
/// # Errors
/// This device's key cannot be read, or the settings refused the write.
pub(super) fn record(paths: &IdentityPaths, stage: Option<Stage>) -> std::io::Result<()> {
    let value = match stage {
        Some(stage) => serde_json::to_value(stage).map_err(std::io::Error::other)?,
        None => Value::Null,
    };
    settings_store::update_section_by(
        SECTION,
        Map::from_iter([(leaf(paths)?, value)]),
        &settings_store::ChangeOrigin::action(ORIGIN),
    )
}

/// Whether the setup of the device whose key `paths` hold is unfinished, as the person's
/// settings say.
#[must_use]
pub fn unfinished(paths: &IdentityPaths) -> bool {
    recorded(paths).is_some()
}
