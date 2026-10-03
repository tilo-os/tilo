//! What this crate provides on each target, as data an app can query.

use std::fmt;

/// One capability of this crate, for [`availability`] and [`Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Copying and pasting text on [`ClipboardTarget::Clipboard`](crate::ClipboardTarget::Clipboard).
    Text,
    /// Reading the clipboard outside a paste the user started (a "Paste"
    /// button, a clipboard watcher).
    ReadAnytime,
    /// Copying images: [`ClipboardBackend::set_image`](crate::ClipboardBackend::set_image).
    Images,
    /// The select-then-middle-click selection:
    /// [`ClipboardTarget::Primary`](crate::ClipboardTarget::Primary) reaching
    /// other apps.
    PrimarySelection,
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

pub(crate) const WEB_READ: &str = "A web page reads the clipboard only while the user pastes.";
pub(crate) const WEB_IMAGES: &str = "Tilo copies only text in the browser.";
pub(crate) const MAC_PRIMARY: &str = "macOS has no middle-click selection.";
pub(crate) const WEB_PRIMARY: &str = "Browsers have no middle-click selection.";

/// Every feature with its availability on Linux, macOS and the web, in that
/// order. A session without a system clipboard (no display) provides less:
/// the in-process fallback keeps copy and paste inside the app.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = {
    use Availability::{Available, Unsupported};
    &[
        (Feature::Text, [Available, Available, Available]),
        (
            Feature::ReadAnytime,
            [Available, Available, Unsupported(WEB_READ)],
        ),
        (
            Feature::Images,
            [Available, Available, Unsupported(WEB_IMAGES)],
        ),
        (
            Feature::PrimarySelection,
            [
                Available,
                Unsupported(MAC_PRIMARY),
                Unsupported(WEB_PRIMARY),
            ],
        ),
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

    const ALL: [Feature; 4] = [
        Feature::Text,
        Feature::ReadAnytime,
        Feature::Images,
        Feature::PrimarySelection,
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
        assert_eq!(availability(Feature::Text), Availability::Available);
        let expected = CAPABILITIES
            .iter()
            .find(|(feature, _)| *feature == Feature::PrimarySelection)
            .map(|(_, targets)| targets[COLUMN]);
        assert_eq!(Some(availability(Feature::PrimarySelection)), expected);
    }
}
