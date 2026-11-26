//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::{Availability, availability, available};

pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    OpenContents,
    SaveContents,
    Paths,
    Launch,
    OpenUri,
}

/// Choosing files to read by contents.
pub(crate) const WEB_PATHS: &str = "This browser can't choose a file by its place on this device.";
pub(crate) const WEB_LAUNCH: &str = "This browser can't open files from this device in other apps.";

impl tilo_platform_caps::Feature for Feature {
    /// Saving bytes where the person chooses; on the web the browser downloads
    /// them.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Unsupported};
        match self {
            Self::OpenContents | Self::SaveContents | Self::OpenUri => {
                [Available, Available, Available]
            }
            Self::Paths => [Available, Available, Unsupported(WEB_PATHS)],
            Self::Launch => [Available, Available, Unsupported(WEB_LAUNCH)],
        }
    }
}
