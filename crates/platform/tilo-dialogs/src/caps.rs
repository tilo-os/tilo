//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::{Availability, availability};

/// A feature this target, or this particular host, cannot provide.
pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

/// One capability of this crate, for [`availability`] and [`Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Choosing files to read by contents:
    /// [`PickRequest::send_contents`](crate::portal::PickRequest::send_contents).
    OpenContents,
    /// Saving bytes where the user chooses:
    /// [`PickRequest::send_save_contents`](crate::portal::PickRequest::send_save_contents).
    /// On the web the browser downloads them.
    SaveContents,
    /// Choosers that answer local paths, folders and open descriptors:
    /// [`PickRequest::send`](crate::portal::PickRequest::send) and friends.
    Paths,
    /// Opening a local file with its default app, or revealing it:
    /// [`LaunchRequest`](crate::portal::LaunchRequest).
    Launch,
    /// Handing a URI to its handler:
    /// [`open_uri`](crate::portal::open_uri). The web opens only `http`,
    /// `https`, `mailto` and `tel`.
    OpenUri,
}

pub(crate) const WEB_PATHS: &str = "A browser page has no local files; choose by contents instead.";
pub(crate) const WEB_LAUNCH: &str = "A browser page cannot open local files in other apps.";

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that
    /// order. A session may provide less at run time (no portal service), never
    /// more.
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_platform_caps::Feature as _;

    #[test]
    fn this_target_reads_its_own_column() {
        assert_eq!(availability(Feature::Paths), Availability::Available);
        let web = Feature::Paths.targets()[2];
        assert_eq!(web, Availability::Unsupported(WEB_PATHS));
    }
}
