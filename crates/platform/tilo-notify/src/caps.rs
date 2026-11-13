//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::{Availability, availability, available};

pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    Show,
    Schedule,
    Activate,
    /// Saying why a notification the app showed went away:
    /// [`set_close_handler`](crate::set_close_handler).
    Close,
    PriorityContacts,
}

pub(crate) const WEB_SCHEDULE: &str = "A closed web page cannot show notifications.";
pub(crate) const MAC_PRIORITY: &str = "macOS decides who breaks through Focus.";
pub(crate) const WEB_PRIORITY: &str = "Browsers decide who breaks through Do Not Disturb.";
pub(crate) const MAC_CLOSE: &str =
    "This macOS backend subscribes to no dismiss action of the notification center.";
pub(crate) const WEB_CLOSE: &str = "Browsers report a notification closing, never why.";

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that order.
    /// `Prompt`: macOS and browsers ask the user before the first notification.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Prompt, Unsupported};
        match self {
            Self::Show | Self::Activate => [Available, Prompt, Prompt],
            Self::Close => [Available, Unsupported(MAC_CLOSE), Unsupported(WEB_CLOSE)],
            Self::Schedule => [Available, Prompt, Unsupported(WEB_SCHEDULE)],
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
