//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::{Availability, availability};

/// A feature this target, or this particular host, cannot provide.
pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

/// One capability of this crate, for [`availability`] and [`Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// One fix: [`request_location`](crate::request_location).
    Locate,
}

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that
    /// order. Every target asks the user first; macOS asks only a bundled app
    /// that says why.
    fn targets(self) -> [Availability; 3] {
        use Availability::Prompt;
        match self {
            Self::Locate => [Prompt, Prompt, Prompt],
        }
    }
}
