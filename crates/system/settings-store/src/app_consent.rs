//! Which app may use another app's actions, as the person answered.
//!
//! An app reaches its own actions and records with its person's authority,
//! and another app's only where the person said it may: the sheet an app's
//! reach raises records the answer here, and the same question is never asked
//! twice.

use serde::{Deserialize, Serialize};

use crate::{Key, SettingsSource};

pub const SECTION: &str = "app_consent";

/// The actions of other apps the person let an app use.
pub const ANSWERS: Key<Vec<Consent>> = Key::new(SECTION, "answers", Vec::new);

/// One app, and the action of another app it may use.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Consent {
    /// The app that asks.
    pub app: String,
    /// The app whose action it uses.
    pub target_app: String,
    /// The resource of `target_app` that publishes the action.
    pub resource: String,
    /// The action's id on `resource`.
    pub action: String,
}

impl Consent {
    #[must_use]
    pub fn new(
        app: impl Into<String>,
        target_app: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            app: app.into(),
            target_app: target_app.into(),
            resource: resource.into(),
            action: action.into(),
        }
    }
}

/// Whether the person let `consent`'s app use the action.
#[must_use]
pub fn allows(source: &dyn SettingsSource, consent: &Consent) -> bool {
    ANSWERS.get_in(source).iter().any(|held| held == consent)
}

/// Remember the person's answer for `consent`.
///
/// # Errors
/// The settings could not be written.
pub fn allow(source: &dyn SettingsSource, consent: &Consent) -> std::io::Result<()> {
    ANSWERS.update_in(source, |mut held| {
        if !held.contains(consent) {
            held.push(consent.clone());
            held.sort();
        }
        held
    })?;
    Ok(())
}

/// Forget the person's answer for `consent`.
///
/// # Errors
/// The settings could not be written.
pub fn revoke(source: &dyn SettingsSource, consent: &Consent) -> std::io::Result<()> {
    ANSWERS.update_in(source, |mut held| {
        held.retain(|kept| kept != consent);
        held
    })?;
    Ok(())
}
