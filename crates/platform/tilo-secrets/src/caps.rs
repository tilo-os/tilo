//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::{Availability, availability};

/// A feature this target, or this particular host, cannot provide.
pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

/// One capability of this crate, for [`availability`] and [`Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Storing, reading and deleting an app's secrets.
    Store,
    /// Secrets held by the operating system's keychain rather than wrapped
    /// by the browser ([`Protection::OsKeychain`](crate::Protection::OsKeychain)).
    OsKeychain,
    /// The `*_blocking` methods.
    Blocking,
}

pub(crate) const WEB_KEYCHAIN: &str =
    "Browsers have no keychain; Tilo encrypts secrets with a key the browser keeps.";
pub(crate) const WEB_BLOCKING: &str = "The browser's storage answers asynchronously.";

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux (and Windows), macOS and the web,
    /// in that order. A Linux session without a Secret Service provides less;
    /// the macOS Keychain may ask the user to allow access.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Unsupported};
        match self {
            Self::Store => [Available, Available, Available],
            Self::OsKeychain => [Available, Available, Unsupported(WEB_KEYCHAIN)],
            Self::Blocking => [Available, Available, Unsupported(WEB_BLOCKING)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_platform_caps::Feature as _;

    #[test]
    fn this_target_reads_its_own_column() {
        assert_eq!(availability(Feature::Store), Availability::Available);
        let web = Feature::Blocking.targets()[2];
        assert_eq!(web, Availability::Unsupported(WEB_BLOCKING));
    }
}
