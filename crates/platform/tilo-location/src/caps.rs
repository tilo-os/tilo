//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::{Availability, availability, available};

pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    Locate,
}

impl tilo_platform_caps::Feature for Feature {
    /// In Linux, macOS and web order. Every target asks the user first; macOS asks
    /// only a bundled app that says why.
    fn targets(self) -> [Availability; 3] {
        use Availability::Prompt;
        match self {
            Self::Locate => [Prompt, Prompt, Prompt],
        }
    }
}
