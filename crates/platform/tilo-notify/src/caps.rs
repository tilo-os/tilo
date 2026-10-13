//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::{Availability, availability, available};

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

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that
    /// order. `Prompt`: macOS and browsers ask the user before the first
    /// notification.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Prompt, Unsupported};
        match self {
            Self::Show => [Available, Prompt, Prompt],
            Self::Schedule => [Available, Prompt, Unsupported(WEB_SCHEDULE)],
            Self::Activate => [Available, Prompt, Unsupported(WEB_ACTIVATE)],
            Self::PriorityContacts => [
                Available,
                Unsupported(MAC_PRIORITY),
                Unsupported(WEB_PRIORITY),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_platform_caps::Feature as _;

    #[test]
    fn this_target_reads_its_own_column() {
        let expected = Feature::Schedule.targets()[tilo_platform_caps::COLUMN];
        assert_eq!(availability(Feature::Schedule), expected);
    }
}
