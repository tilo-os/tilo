//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::{Availability, availability, available};

pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    Text,
    ReadAnytime,
    Images,
    PrimarySelection,
}

pub(crate) const WEB_READ: &str = "A web page reads the clipboard only while the user pastes.";
pub(crate) const WEB_IMAGES: &str = "Tilo copies only text in the browser.";
pub(crate) const MAC_PRIMARY: &str = "macOS has no middle-click selection.";
pub(crate) const WEB_PRIMARY: &str = "Browsers have no middle-click selection.";

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that
    /// order. A session without a system clipboard (no display) provides less:
    /// the in-process fallback keeps copy and paste inside the app.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Unsupported};
        match self {
            Self::Text => [Available, Available, Available],
            Self::ReadAnytime => [Available, Available, Unsupported(WEB_READ)],
            Self::Images => [Available, Available, Unsupported(WEB_IMAGES)],
            Self::PrimarySelection => [
                Available,
                Unsupported(MAC_PRIMARY),
                Unsupported(WEB_PRIMARY),
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
        assert_eq!(availability(Feature::Text), Availability::Available);
        let expected = Feature::PrimarySelection.targets()[tilo_platform_caps::COLUMN];
        assert_eq!(availability(Feature::PrimarySelection), expected);
    }
}
