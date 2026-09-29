//! What this crate provides on each target, as data an app can query.

use std::fmt;

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

pub(crate) const WEB_PATHS: &str = "A browser page has no local files; choose by contents instead.";
pub(crate) const WEB_LAUNCH: &str = "A browser page cannot open local files in other apps.";

/// Every feature with its availability on Linux, macOS and the web, in that
/// order. A session may provide less at run time (no portal service), never
/// more.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = {
    use Availability::{Available, Unsupported};
    &[
        (Feature::OpenContents, [Available, Available, Available]),
        (Feature::SaveContents, [Available, Available, Available]),
        (
            Feature::Paths,
            [Available, Available, Unsupported(WEB_PATHS)],
        ),
        (
            Feature::Launch,
            [Available, Available, Unsupported(WEB_LAUNCH)],
        ),
        (Feature::OpenUri, [Available, Available, Available]),
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

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Feature; 5] = [
        Feature::OpenContents,
        Feature::SaveContents,
        Feature::Paths,
        Feature::Launch,
        Feature::OpenUri,
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
        assert_eq!(availability(Feature::Paths), Availability::Available);
        let web = CAPABILITIES
            .iter()
            .find(|(feature, _)| *feature == Feature::Paths)
            .map(|(_, targets)| targets[2]);
        assert_eq!(web, Some(Availability::Unsupported(WEB_PATHS)));
    }
}
