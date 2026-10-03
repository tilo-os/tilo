//! What this crate provides on each target, as data an app can query.

use std::fmt;

/// One capability of this crate, for [`availability`] and [`Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Showing, replacing and withdrawing notifications while the app runs.
    Show,
    /// Handing a notification to the platform for later, including after the
    /// app exits: [`Notifier::schedule`](crate::Notifier::schedule).
    Schedule,
    /// Handing a clicked notification's activation URI to the app:
    /// [`set_activation_handler`](crate::set_activation_handler).
    Activate,
    /// Naming who may break through Do Not Disturb:
    /// [`publish_priority_contacts`](crate::publish_priority_contacts).
    PriorityContacts,
}

/// Whether a [`Feature`] can be used on a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Availability {
    Available,
    /// Usable after a user prompt (browser permission, macOS TCC).
    Prompt,
    /// Never on this target; the reason is user-facing.
    Unsupported(&'static str),
}

/// A feature this target, or this particular host, cannot provide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Unsupported {
    pub feature: Feature,
    /// User-facing.
    pub reason: &'static str,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason)
    }
}

impl std::error::Error for Unsupported {}

pub(crate) const LINUX_SCHEDULE: &str =
    "Linux shows notifications only while the app that sends them runs.";
pub(crate) const WEB_SCHEDULE: &str = "A closed web page cannot show notifications.";
pub(crate) const WEB_ACTIVATE: &str = "A click on a web notification only focuses the page.";
pub(crate) const MAC_PRIORITY: &str = "macOS decides who breaks through Focus.";
pub(crate) const WEB_PRIORITY: &str = "Browsers decide who breaks through Do Not Disturb.";

/// Every feature with its availability on Linux, macOS and the web, in that
/// order. `Prompt`: macOS and browsers ask the user before the first
/// notification.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = {
    use Availability::{Available, Prompt, Unsupported};
    &[
        (Feature::Show, [Available, Prompt, Prompt]),
        (
            Feature::Schedule,
            [
                Unsupported(LINUX_SCHEDULE),
                Prompt,
                Unsupported(WEB_SCHEDULE),
            ],
        ),
        (
            Feature::Activate,
            [Available, Prompt, Unsupported(WEB_ACTIVATE)],
        ),
        (
            Feature::PriorityContacts,
            [
                Available,
                Unsupported(MAC_PRIORITY),
                Unsupported(WEB_PRIORITY),
            ],
        ),
    ]
};

/// This target's column of [`CAPABILITIES`]. Native targets other than
/// macOS report the Linux column.
const COLUMN: usize = if cfg!(all(target_arch = "wasm32", target_os = "unknown")) {
    2
} else if cfg!(target_os = "macos") {
    1
} else {
    0
};

/// Whether `feature` can be used on this target.
pub fn availability(feature: Feature) -> Availability {
    CAPABILITIES
        .iter()
        .find(|(listed, _)| *listed == feature)
        .and_then(|(_, targets)| targets.get(COLUMN).copied())
        .unwrap_or(Availability::Unsupported(
            "This build does not know the feature.",
        ))
}

/// The error for `feature` on this target.
pub(crate) fn unsupported(feature: Feature) -> Unsupported {
    let reason = match availability(feature) {
        Availability::Unsupported(reason) => reason,
        Availability::Available | Availability::Prompt => "This host cannot do this.",
    };
    Unsupported { feature, reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Feature; 4] = [
        Feature::Show,
        Feature::Schedule,
        Feature::Activate,
        Feature::PriorityContacts,
    ];

    #[test]
    fn every_feature_is_listed_once() {
        for feature in ALL {
            let rows = CAPABILITIES.iter().filter(|(listed, _)| *listed == feature);
            assert_eq!(rows.count(), 1, "{feature:?}");
        }
        assert_eq!(CAPABILITIES.len(), ALL.len());
    }

    #[test]
    fn this_target_reads_its_own_column() {
        let expected = CAPABILITIES
            .iter()
            .find(|(feature, _)| *feature == Feature::Schedule)
            .map(|(_, targets)| targets[COLUMN]);
        assert_eq!(Some(availability(Feature::Schedule)), expected);
    }
}
