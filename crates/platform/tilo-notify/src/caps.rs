//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::Availability;

/// A feature this target, or this particular host, cannot provide.
pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

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
            [Available, Prompt, Unsupported(WEB_SCHEDULE)],
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

/// Whether `feature` can be used on this target.
pub fn availability(feature: Feature) -> Availability {
    tilo_platform_caps::availability(CAPABILITIES, feature)
}

/// The error for `feature` on this target.
pub(crate) fn unsupported(feature: Feature) -> Unsupported {
    Unsupported::of(CAPABILITIES, feature)
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
            .map(|(_, targets)| targets[tilo_platform_caps::COLUMN]);
        assert_eq!(Some(availability(Feature::Schedule)), expected);
    }
}
