//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::Availability;

/// A feature this target, or this particular host, cannot provide.
pub type Unsupported = tilo_platform_caps::Unsupported<Feature>;

/// One capability of this crate, for [`availability`] and [`Unsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Reading, crash-safe replacement, no-clobber creation, removal,
    /// directories and listings inside a [`Dir`](crate::Dir).
    Files,
    /// [`Dir::rename_noreplace`](crate::Dir::rename_noreplace). The web
    /// emulates it under a Web Lock held by every `tilo-files` writer that
    /// creates names.
    RenameNoReplace,
    /// [`Dir::lock`](crate::Dir::lock): `flock` natively, Web Locks (per
    /// origin, across tabs) on the web.
    Locks,
    /// The synchronous [`blocking`](crate::blocking) operations.
    Blocking,
    /// [`Dir::native_path`](crate::Dir::native_path).
    NativePaths,
    /// Permission bits and symbolic links in [`Meta`](crate::Meta).
    ModesAndLinks,
    /// Change notifications for a directory.
    Watch,
    /// Moving entries to the user's trash: [`Dir::trash`](crate::Dir::trash).
    Trash,
    /// Owner-only directories with owner and access checks.
    Private,
    /// Extended attributes.
    Attributes,
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(crate) const WEB_BLOCKING: &str =
    "In a browser only app data, settings and state the page has loaded answer at once.";
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(crate) const WEB_BLOCKING_LOCKS: &str = "A browser page cannot wait for a lock synchronously.";
pub(crate) const NO_DISK_WATCH: &str = "This build cannot watch folders.";
/// Native watching needs the `watch` feature.
const DISK_WATCH: Availability = if cfg!(feature = "watch") {
    Availability::Available
} else {
    Availability::Unsupported(NO_DISK_WATCH)
};
pub(crate) const WEB_NATIVE_PATHS: &str = "Browser storage has no file system paths.";
pub(crate) const MEMORY_NATIVE_PATHS: &str = "Storage kept in memory has no file system path.";
pub(crate) const WEB_MODES: &str = "Browser storage has no permission bits or links.";
pub(crate) const NOT_YET: &str = "Tilo cannot do this yet.";

/// Every feature with its availability on Linux, macOS and the web, in that
/// order. A [`Dir`](crate::Dir) kept in memory provides less than its
/// target's column: no native paths, modes or links. On the web, blocking
/// calls work on the app locations the host mirrored
/// ([`web::mirror`](crate::web)), except for locks.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = {
    use Availability::{Available, Unsupported};
    &[
        (Feature::Files, [Available, Available, Available]),
        (Feature::RenameNoReplace, [Available, Available, Available]),
        (Feature::Locks, [Available, Available, Available]),
        (Feature::Blocking, [Available, Available, Available]),
        (
            Feature::NativePaths,
            [Available, Available, Unsupported(WEB_NATIVE_PATHS)],
        ),
        (
            Feature::ModesAndLinks,
            [Available, Available, Unsupported(WEB_MODES)],
        ),
        (Feature::Watch, [DISK_WATCH, DISK_WATCH, Available]),
        (
            Feature::Trash,
            [
                Unsupported(NOT_YET),
                Unsupported(NOT_YET),
                Unsupported(NOT_YET),
            ],
        ),
        (
            Feature::Private,
            [
                Unsupported(NOT_YET),
                Unsupported(NOT_YET),
                Unsupported(NOT_YET),
            ],
        ),
        (
            Feature::Attributes,
            [
                Unsupported(NOT_YET),
                Unsupported(NOT_YET),
                Unsupported(NOT_YET),
            ],
        ),
    ]
};

/// Whether `feature` can be used on this target.
pub fn availability(feature: Feature) -> Availability {
    tilo_platform_caps::availability(CAPABILITIES, feature)
}

/// The error for `feature` on this target, when its column says it is
/// unsupported.
pub(crate) fn unsupported(feature: Feature) -> Unsupported {
    Unsupported::of(CAPABILITIES, feature)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Feature; 10] = [
        Feature::Files,
        Feature::RenameNoReplace,
        Feature::Locks,
        Feature::Blocking,
        Feature::NativePaths,
        Feature::ModesAndLinks,
        Feature::Watch,
        Feature::Trash,
        Feature::Private,
        Feature::Attributes,
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
        assert_eq!(availability(Feature::Blocking), Availability::Available);
        assert_eq!(availability(Feature::Watch), DISK_WATCH);
        let web = CAPABILITIES
            .iter()
            .find(|(feature, _)| *feature == Feature::NativePaths)
            .map(|(_, targets)| targets[2]);
        assert_eq!(web, Some(Availability::Unsupported(WEB_NATIVE_PATHS)));
    }
}
